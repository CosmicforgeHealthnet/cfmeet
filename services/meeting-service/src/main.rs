use axum::Router;
use backend::config::logging;
use dotenvy::dotenv;
use sea_orm::{ConnectOptions, Database};

use backend::{
    config::{AppConfig, EmailConfig, LiveKitConfig, RedisConfig},
    routes,
    services::RedisService,
    state::AppState,
    swagger,
    workers::{
        outbox_relay::{spawn_outbox_relay, OutboxRelay},
        spawn_conference_scheduler, spawn_email_worker, spawn_meeting_auto_end_worker,
        spawn_meeting_auto_start_worker,
    },
};

/// `DATABASE_MAX_CONNECTIONS` (2-200, default 10).
fn database_max_connections() -> u32 {
    std::env::var("DATABASE_MAX_CONNECTIONS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(10)
        .clamp(2, 200)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load environment variables
    dotenv().ok();

    // Initialize logging (terminal + file)
    let _guard = logging::init_logging();

    // Load app configuration
    let app_config = AppConfig::from_env().expect("Failed to load app configuration");

    // Connect to database with connection pool settings
    tracing::info!("Connecting to database...");
    let mut db_opts = ConnectOptions::new(&app_config.database_url);
    db_opts
        // B6: `DATABASE_MAX_CONNECTIONS` (2-200, default 10) sizes the pool per deployment;
        // production raises it only through the release operation `activate-database-pools`,
        // which checks the database's connection budget first.
        .max_connections(database_max_connections())
        .min_connections(2)
        .connect_timeout(std::time::Duration::from_secs(30))
        .acquire_timeout(std::time::Duration::from_secs(30))
        .idle_timeout(std::time::Duration::from_secs(300))
        .max_lifetime(std::time::Duration::from_secs(1800))
        .sqlx_logging(false)
        // DATA-001: legacy tables keep naive `TIMESTAMP` columns that hold UTC. Pin every
        // session to UTC explicitly (a later `-c` wins over any `options` in the URL), so
        // `DEFAULT CURRENT_TIMESTAMP`, `now()` comparisons and `AT TIME ZONE` casts never
        // follow a server or URL default.
        .map_sqlx_postgres_opts(|o| o.options([("TimeZone", "UTC")]));
    let db = Database::connect(db_opts).await?;
    backend::utils::datetime::assert_session_utc(&db)
        .await
        .expect("database session time zone must be UTC (DATA-001)");
    tracing::info!("✅ Database connected successfully");

    // DATA-011 target rule for the base history: applied ⊇ known, unknown versions fail
    // closed (the additive histories below follow the same rule).
    if !migration::base_ready(&db)
        .await
        .expect("Base migration readiness")
    {
        panic!("Base migrations are pending (migration up)");
    }

    // Load email configuration (unified email delivery). A selected provider with missing or
    // malformed settings stops startup (fail closed): mail must never be silently dropped.
    let email_config =
        EmailConfig::from_env().unwrap_or_else(|e| panic!("email configuration error: {e}"));
    tracing::info!(
        "✅ Email configuration loaded (provider: {}, sender: {})",
        email_config.provider.as_str(),
        email_config.from_email
    );

    // Load LiveKit configuration
    let livekit_config = LiveKitConfig::from_env()
        .expect("Failed to load LiveKit configuration. Set LIVEKIT_URL, LIVEKIT_API_KEY, and LIVEKIT_API_SECRET.");
    tracing::info!("✅ LiveKit configuration loaded ({})", livekit_config.url);

    // Load Redis configuration and create service
    let redis_config = RedisConfig::from_env().expect("Failed to load Redis configuration");
    tracing::info!("Connecting to Redis at {}...", redis_config.url);
    let redis_service = RedisService::new(&redis_config)
        .await
        .expect("Failed to connect to Redis");
    tracing::info!("✅ Redis connected successfully");

    // Create application state
    let mut state = AppState::new(
        db.clone(),
        redis_service,
        &app_config,
        &email_config,
        &livekit_config,
    );

    // Part VI §51 / §53: platform credential access (MEETING_PLATFORM_ACCESS_ENABLED) and
    // environment-scoped public events (MEETING_SERVICE_PUBLIC_EVENTS_DESTINATION).
    // Enabled but misconfigured stops startup (fail closed).
    let platform = backend::services::platform::PlatformConfig::from_env()
        .unwrap_or_else(|e| panic!("platform access configuration error: {e}"));
    let mut _public_event_publisher = None;
    if let Some(config) = platform {
        let client = config
            .client()
            .unwrap_or_else(|e| panic!("platform access configuration error: {e}"));
        if let Some(receiver) = config
            .receiver(client.clone())
            .unwrap_or_else(|e| panic!("platform access configuration error: {e}"))
        {
            state = state.with_platform(receiver);
            tracing::info!("Part VI: platform credential access to /api/v2 enabled");
        }
        // ADR 0062: partner users join with their CF Meet user token (and partner meeting
        // chat). Their tables live in an additive history (`platform-build-migration up`);
        // refuse to start without it rather than failing joins.
        if let Some(users) = config.platform_users(&db, client.clone()) {
            if !migration::platform_build_ready(&db)
                .await
                .expect("Platform-build migration readiness")
            {
                panic!("Platform-build migration is required while MEETING_PLATFORM_USER_JOIN_ENABLED is on (platform-build-migration up)");
            }
            if let Some(chat) = users.chat.clone() {
                backend::services::platform::partner_chat::spawn_partner_chat_sync(
                    chat,
                    config.chat_sync_interval,
                );
                tracing::info!("ADR 0062: partner meeting chat enabled");
            }
            state = state.with_platform_users(users);
            tracing::info!("ADR 0062: partner user join (CF Meet user tokens) enabled");
        }
        // Part VI §55: usage facts of environment-owned meetings -> unified usage ledger.
        if let Some(s) = config.usage_export.clone() {
            use backend::services::platform::usage_export;
            let exporter = cf_usage_facts::exporter::Exporter::new(
                std::sync::Arc::new(cf_usage_facts::exporter::DeveloperPlatformSink::new(
                    client.clone(),
                )),
                s,
            )
            .with_events(std::sync::Arc::new(usage_export::MeetingFacts::new(
                db.clone(),
            )))
            .with_intervals(std::sync::Arc::new(usage_export::ArtifactStorage::new(
                db.clone(),
            )));
            tokio::spawn(std::sync::Arc::new(exporter).run());
            tracing::info!("Part VI: meeting usage facts -> Developer Platform");
        }
        if config.destination
            == backend::services::platform::PublicEventsDestination::DeveloperPlatform
        {
            use backend::services::platform::events;
            events::set_staging(true);
            _public_event_publisher = Some(events::spawn_public_event_publisher(
                events::PublicEventPublisher::new(db.clone(), client, config.runtime),
                config.publish_interval,
            ));
            tracing::info!("Part VI: environment-scoped meeting events -> Developer Platform");
        }
    }
    // ADR 0064 / 0065: CF Meet Apps (MEETING_OAUTH_AUTHORIZE_ENABLED,
    // MEETING_DELEGATED_APPS_ENABLED, MEETING_IN_MEETING_APPS_ENABLED; all off by default).
    // Enabled but misconfigured stops startup (fail closed). No migration of their own.
    let apps_config = backend::services::apps::AppsConfig::from_env()
        .unwrap_or_else(|e| panic!("apps configuration error: {e}"));
    let mut userinfo_verifier = None;
    if let Some(config) = apps_config {
        if config.delegated_enabled && state.platform.is_none() {
            panic!("apps configuration error: MEETING_DELEGATED_APPS_ENABLED requires the platform credential receiver (MEETING_PLATFORM_ACCESS_ENABLED)");
        }
        let apps = config
            .build(state.meeting.core.organization_authority.clone())
            .unwrap_or_else(|e| panic!("apps configuration error: {e}"));
        userinfo_verifier = config
            .userinfo_verifier()
            .unwrap_or_else(|e| panic!("apps configuration error: {e}"));
        tracing::info!(
            authorize = config.authorize_enabled,
            delegated = config.delegated_enabled,
            in_meeting = config.in_meeting_enabled,
            organization_installs = apps.organizations.is_some(),
            "ADR 0064 / 0065: CF Meet Apps enabled"
        );
        state = state.with_apps(apps);
    }
    tracing::info!("✅ Hosted UI URL: {}", app_config.app_url);

    // Blueprint Section 2: account sessions, the revocation epoch and the security audit
    // live in their own additive history (`account-security-migration up`). Refuse to run
    // without them rather than silently skipping revocation.
    if !migration::account_security_ready(&state.db)
        .await
        .expect("Account security migration readiness")
    {
        panic!("Account security migration is required (account-security-migration up)");
    }
    if !migration::concurrent_devices_ready(&state.db)
        .await
        .expect("Concurrent-device migration readiness")
    {
        panic!("Concurrent-device migration is required (meeting-concurrent-devices-migration up)");
    }

    // Blueprint Section 10: idempotency records and agenda indexes live in their own
    // additive history (`meetings-calendar-migration up`).
    if !migration::meetings_calendar_ready(&state.db)
        .await
        .expect("Meetings calendar migration readiness")
    {
        panic!("Meetings calendar migration is required (meetings-calendar-migration up)");
    }

    // Blueprint Sections 9 and 11: profile columns on `users` and notification jobs live
    // in their own additive history (`profile-notifications-migration up`).
    if !migration::profile_notifications_ready(&state.db)
        .await
        .expect("Profile/notifications migration readiness")
    {
        panic!("Profile/notifications migration is required (profile-notifications-migration up)");
    }

    // Blueprint Sections 2-3 hardening: hashed legacy API keys and webhook secrets live in
    // their own additive history (`account-hardening-migration up`).
    if !migration::account_hardening_ready(&state.db)
        .await
        .expect("Account hardening migration readiness")
    {
        panic!("Account hardening migration is required (account-hardening-migration up)");
    }

    // Backend gap audit: case-insensitive identity, authenticator MFA, avatars and calendar
    // invites live in their own additive history (`gap-closure-migration up`).
    if !migration::gap_closure_ready(&state.db)
        .await
        .expect("Gap-closure migration readiness")
    {
        panic!("Gap-closure migration is required (gap-closure-migration up)");
    }

    // S2: account deletion queues durable erasure jobs for Messaging, the Hub and Calling
    // (`account-privacy-migration up`); the privacy worker delivers and retries them.
    if !migration::account_privacy_ready(&state.db)
        .await
        .expect("Account-privacy migration readiness")
    {
        panic!("Account-privacy migration is required (account-privacy-migration up)");
    }
    backend::services::account_privacy::spawn(state.db.clone(), std::time::Duration::from_secs(60));
    if !migration::account_phone_ready(&state.db)
        .await
        .expect("Account-phone migration readiness")
    {
        panic!("Account-phone migration is required (account-phone-migration up)");
    }
    if std::env::var("PHONE_ACCOUNTS_ENABLED").ok().as_deref() == Some("true") {
        backend::services::account_phone::TwilioVerify::from_env().unwrap_or_else(|_| {
            panic!("Phone accounts enabled without a valid verification provider")
        });
    }

    // Blueprint Section 4: which store answers organization questions (the client
    // configuration was validated when the meeting domain was built; misconfiguration
    // stopped startup there).
    tracing::info!(
        "Organizations authority mode: {}",
        state
            .meeting
            .core
            .organization_authority
            .as_ref()
            .map(|a| a.mode().as_str())
            .unwrap_or("legacy")
    );

    state.communications = backend::services::communications::Bridge::from_env()
        .unwrap_or_else(|e| panic!("Communications configuration error: {e}"));
    // Blueprint Section 11: recipient notification preferences live in the Hub when it is
    // configured (signed read); otherwise no preference exists.
    if let Some(bridge) = state.communications.clone() {
        state.notifier = std::sync::Arc::new(backend::services::notifications::Notifier::new(
            state.email_service.clone(),
            std::sync::Arc::new(backend::services::notifications::HubPreferences::new(
                bridge,
            )),
        ));
    }
    if state.communications.is_some() {
        if !migration::communications_ready(&state.db)
            .await
            .expect("Communications migration readiness")
        {
            panic!("Account Communications integration migration is required");
        }
        if !migration::communications_facade_ready(&state.db)
            .await
            .expect("Communications facade migration readiness")
        {
            panic!("Account Communications facade migration is required");
        }
        let communications_state = state.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_millis(500));
            loop {
                tick.tick().await;
                if backend::services::communications::deliver_due(&communications_state)
                    .await
                    .is_err()
                {
                    tracing::warn!("Account Communications events deferred");
                }
            }
        });
    }

    // Gap audit A10: sign-ins past their absolute lifetime end everywhere (once a minute).
    {
        let sweep_state = state.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                tick.tick().await;
                match backend::services::account_security::sweep_expired_sessions(&sweep_state)
                    .await
                {
                    Ok(0) => {}
                    Ok(n) => tracing::info!("ended {n} expired account sign-in(s)"),
                    Err(e) => tracing::warn!("expired sign-in sweep deferred: {e}"),
                }
            }
        });
    }

    // Unified email delivery: the durable ledger lives in its own additive history.
    if !migration::lifecycle_delivery_ready(&state.db)
        .await
        .expect("Lifecycle/delivery migration readiness")
    {
        panic!("Lifecycle/delivery migration is required (lifecycle-delivery-migration up)");
    }
    // Messages left in the former volatile Redis list move to the durable ledger once.
    match backend::services::email_delivery::import_legacy_redis_queue(
        &state.db,
        &backend::queues::RedisEmailQueue::new(state.redis.clone()),
    )
    .await
    {
        Ok(0) => {}
        Ok(n) => tracing::info!("imported {n} queued legacy email jobs into the email ledger"),
        Err(e) => tracing::warn!("legacy email queue import deferred: {e}"),
    }
    let _email_worker = match backend::services::email_delivery::provider_from_config(&email_config)
    {
        Ok(Some(provider)) => {
            let worker = backend::services::email_delivery::EmailWorker::new(
                state.db.clone(),
                provider,
                backend::services::email_delivery::WorkerSettings::from_config(&email_config),
            );
            tracing::info!("✅ Email worker started");
            Some(spawn_email_worker(
                worker,
                std::time::Duration::from_millis(email_config.poll_millis),
            ))
        }
        Ok(None) => {
            tracing::warn!("ℹ️ No email provider configured (EMAIL_PROVIDER): emails stay queued");
            None
        }
        Err(e) => panic!("email provider error: {e}"),
    };

    // Notification Service N7 / N11: the service's public HPKE key seals invitation tokens /
    // addresses into published events (unset: invitation emails stay on the legacy path).
    let sealing = backend::services::notification_publications::sealing_key_from_env()
        .unwrap_or_else(|e| panic!("notification sealing key configuration error: {e}"));
    // Notification Service N12: the CRITICAL lane seals codes to the same service key and
    // each message's legacy email to the Account fallback key (CF_ACCOUNT_FALLBACK_KEY_FILE,
    // self-tested here; unset: every CRITICAL message stays on the legacy path).
    let critical_keys = backend::services::critical_notifications::keys_from_env(sealing.clone())
        .unwrap_or_else(|e| panic!("CRITICAL notification lane configuration error: {e}"));
    if critical_keys.fallback.is_some() {
        tracing::info!("CRITICAL notification lane: Account fallback key loaded and self-tested");
    } else {
        tracing::info!(
            "CF_ACCOUNT_FALLBACK_KEY_FILE is not set: CRITICAL notifications stay on the legacy email path"
        );
    }
    backend::services::critical_notifications::install(critical_keys);
    if sealing.is_some() {
        state.notifier = std::sync::Arc::new(
            backend::services::notifications::Notifier::new(
                state.notifier.mail.clone(),
                state.notifier.preferences.clone(),
            )
            .with_sealing(sealing),
        );
    }
    // The relay of Meeting's publications (shadow copies in SHADOW, publications of NEW
    // decisions) to I1: posts only with CF_NOTIFICATIONS_BASE_URL; the routing mode alone
    // decides what is enqueued. Expiry and purge run in every case.
    let publication_sink =
        backend::services::notification_publications::NotificationServiceEvents::from_env()
            .unwrap_or_else(|e| panic!("notification publication relay configuration error: {e}"))
            .map(|s| {
                std::sync::Arc::new(s)
                    as std::sync::Arc<
                        dyn backend::services::notification_publications::NotificationEventSink,
                    >
            });
    if publication_sink.is_some() {
        tracing::info!("Notification publications are relayed to the Notification Service");
    } else {
        tracing::info!(
            "CF_NOTIFICATIONS_BASE_URL is not set: notification publications are not relayed"
        );
    }
    // N12 PW1: the CRITICAL lane relay (I1 posts, AUTO / FORCED failover to the legacy
    // email path, expiry). Fails over even without CF_NOTIFICATIONS_BASE_URL.
    tokio::spawn(backend::services::critical_notifications::run(
        state.db.clone(),
        publication_sink.clone(),
        std::time::Duration::from_secs(2),
    ));
    tokio::spawn(backend::services::notification_publications::run(
        state.db.clone(),
        publication_sink,
        std::time::Duration::from_secs(5),
    ));

    // Blueprint Section 11: meeting notifications (reschedule/cancel notices, reminders).
    // Off unless NOTIFICATIONS_EMAIL_ENABLED=true; emails still need the email worker above.
    let notification_context =
        backend::services::notifications::NotificationContext::from_state(&state);
    let _notification_worker = if notification_context.config.email_enabled {
        if email_config.provider == backend::config::EmailProviderKind::None {
            tracing::warn!(
                "Notification email is enabled but no email provider is configured: emails stay queued"
            );
        }
        Some(backend::services::notifications::spawn_worker(
            notification_context,
        ))
    } else {
        tracing::info!("ℹ️ Notification email disabled (NOTIFICATIONS_EMAIL_ENABLED)");
        None
    };

    // Start meeting auto-end worker (always runs)
    let _meeting_auto_end_worker = spawn_meeting_auto_end_worker(
        db.clone(),
        Some(state.meeting.core.rtc.clone()),
        Some(std::sync::Arc::new(
            backend::services::compat::CompatService::new(state.meeting.core.clone()),
        )),
    );
    tracing::info!("✅ Meeting auto-end worker started");

    // Start meeting auto-start worker (always runs)
    let _meeting_auto_start_worker = spawn_meeting_auto_start_worker(
        db.clone(),
        Some(std::sync::Arc::new(
            backend::services::compat::CompatService::new(state.meeting.core.clone()),
        )),
    );
    tracing::info!("✅ Meeting auto-start worker started");

    // Phase 2 meeting domain workers (v2 conferences only; legacy workers above keep
    // serving legacy meetings).
    let flags = state.meeting.core.flags.clone();
    tracing::info!(
        "Meeting v2: enabled={} waiting_room_v2={} rtc_provider_control_v2={} legacy_write_disabled={}",
        flags.meeting_v2_enabled,
        flags.waiting_room_v2_enabled,
        flags.rtc_provider_control_v2,
        flags.legacy_meeting_write_disabled
    );
    let _conference_scheduler = flags
        .meeting_v2_enabled
        .then(|| spawn_conference_scheduler(state.meeting.clone()));
    // Final completion: empty LIVE instant conferences end after 10 minutes without anyone
    // (CONFERENCE_EMPTY_TIMEOUT_SECS) and archived spaces never keep a LIVE conference.
    let _empty_conference_sweeper = flags.meeting_v2_enabled.then(|| {
        let timeout = backend::workers::empty_conference::empty_timeout_from_env();
        tracing::info!(
            "Empty instant conference timeout: {}s",
            timeout.num_seconds()
        );
        backend::workers::spawn_empty_conference_sweeper(
            backend::workers::EmptyConferenceSweeper::new(state.meeting.clone(), timeout),
            30,
        )
    });
    // Part E: the outbox relay delivers client realtime events to Redis fanout (when
    // `conference_realtime_enabled`) and executes provider commands through the RTC
    // control plane (when `rtc_provider_control_v2`). Without a consumer, rows stay
    // durably PENDING in PostgreSQL.
    let _outbox_relay =
        (flags.conference_realtime_enabled || flags.rtc_provider_control_v2).then(|| {
            let publisher: Option<
                std::sync::Arc<dyn backend::workers::outbox_relay::EventPublisher>,
            > = flags.conference_realtime_enabled.then(|| {
                std::sync::Arc::new(backend::services::realtime::RedisPublisher::new(
                    state.redis.clone(),
                ))
                    as std::sync::Arc<dyn backend::workers::outbox_relay::EventPublisher>
            });
            let control = flags.rtc_provider_control_v2.then(|| {
                std::sync::Arc::new(backend::services::rtc_control::RtcControl::new(
                    state.meeting.core.clone(),
                ))
            });
            spawn_outbox_relay(OutboxRelay::with_control(db.clone(), publisher, control), 2)
        });
    // Part F: restart-safe breakout countdown completion.
    let _breakout_closer = flags.collaboration_v2_enabled.then(|| {
        backend::workers::spawn_breakout_closer(
            backend::workers::BreakoutCloser::new(state.meeting.core.clone()),
            2,
        )
    });
    // Part G: artefact finalization retries + retention deletion (row-leased).
    let _artifact_worker =
        (flags.recording_v2_enabled || flags.transcription_v2_enabled).then(|| {
            let poll = state.meeting.core.config.artifacts.retention_poll_secs;
            backend::services::artifacts::retention::spawn_artifact_worker(
                backend::services::artifacts::retention::ArtifactRetentionWorker::new(
                    state.meeting.core.clone(),
                ),
                poll,
            )
        });
    let _conference_reconciler = flags.rtc_provider_control_v2.then(|| {
        let poll = state.meeting.core.config.reconciler_poll_secs;
        backend::workers::spawn_conference_reconciler(
            backend::workers::ConferenceReconciler::new(state.meeting.core.clone()),
            poll,
        )
    });
    // Part V H: Unified Messaging integration (FEATURE_UNIFIED_MESSAGING).
    let _unified_messaging_sync = state.meeting.core.unified.clone().map(|um| {
        backend::workers::unified_messaging_sync::spawn_unified_messaging_sync(
            um,
            backend::workers::unified_messaging_sync::sync_interval(),
        )
    });
    tracing::info!(
        "Part E: realtime={} provider_control={} provider={} capabilities={:?}",
        flags.conference_realtime_enabled,
        flags.rtc_provider_control_v2,
        state.meeting.core.rtc.name(),
        state.meeting.core.rtc.capabilities()
    );

    // Configure CORS - allow frontend(s) to access API
    tracing::info!(
        "🔒 CORS configured for origins: {:?}",
        app_config.frontend_urls
    );
    let cors = backend::routes::cors_layer(&app_config.frontend_urls);

    // Start server
    let addr = app_config.server_addr();

    // Part VI §50: Developer Console session bridge (DEVELOPER_SESSION_BRIDGE_ENABLED).
    let session_bridge = backend::services::session_bridge::SessionBridgeConfig::from_env()
        .and_then(|c| c.map(|c| c.verifier()).transpose())
        .unwrap_or_else(|e| panic!("session bridge configuration error: {e}"));
    let bridge_router = session_bridge
        .map(|verifier| backend::services::session_bridge::router(state.clone(), verifier));
    if bridge_router.is_some() {
        tracing::info!("Part VI: Developer Console session bridge enabled");
    }
    // ADR 0064: the internal OAuth userinfo route (meeting-oauth-userinfo.read).
    let userinfo_router = userinfo_verifier
        .map(|verifier| backend::services::apps::userinfo::router(state.clone(), verifier));

    // Document 4 §3.1: Notification Service user relay (CF_NOTIFICATIONS_BASE_URL; unset:
    // the /api/v2/notifications routes answer 503 and nothing is forwarded).
    state.notifications = backend::services::notification_relay::NotificationsRelay::from_env()
        .unwrap_or_else(|e| panic!("notification relay configuration error: {e}"));
    if state.notifications.is_some() {
        tracing::info!("Notification Service user relay enabled");
    }

    // Notification Service owner APIs X1 / X2 (NOTIFICATION_OWNER_API_ENABLED, Document 4
    // §3.4). Reads only: no migration of their own.
    let notification_owner =
        backend::services::notification_owner::NotificationOwnerConfig::from_env()
            .and_then(|c| c.map(|c| c.verifier()).transpose())
            .unwrap_or_else(|e| panic!("notification owner API configuration error: {e}"));
    let owner_router = notification_owner
        .map(|verifier| backend::services::notification_owner::router(state.clone(), verifier));
    if owner_router.is_some() {
        tracing::info!("Notification Service owner APIs enabled");
    }

    // ADR 0068: the admin bridge's operator reads (ADMIN_OPERATOR_API_ENABLED, default off).
    let admin_operator = backend::services::admin_operator::AdminOperatorConfig::from_env()
        .and_then(|c| c.map(|c| c.verifier()).transpose())
        .unwrap_or_else(|e| panic!("admin operator API configuration error: {e}"));
    let operator_router = admin_operator
        .map(|verifier| backend::services::admin_operator::router(state.clone(), verifier));
    if operator_router.is_some() {
        // A4–A7: the bridge's commands record themselves in `meeting_admin_operations` and
        // lock accounts by `users.account_version` (`admin-operations-migration up`).
        if !migration::admin_operations_ready(&state.db)
            .await
            .expect("Admin-operations migration readiness")
        {
            panic!("Admin-operations migration is required (admin-operations-migration up)");
        }
        tracing::info!("admin bridge operator reads and commands enabled");
        // A7: owner-delivered exports: builds that lapsed are retried, copies past their
        // 7-day retention (or of a deleted account) are purged.
        let _account_export_worker = backend::services::account_export::spawn_worker(
            state.clone(),
            std::time::Duration::from_secs(60),
        );
    }

    // Final closure: Organizations event ingress (ORGANIZATION_EVENTS_ENABLED). Its tables
    // live in their own additive history (`organization-integration-migration up`).
    let organization_events =
        backend::services::organization_events::OrganizationEventsConfig::from_env()
            .and_then(|c| c.map(|c| c.verifier()).transpose())
            .unwrap_or_else(|e| panic!("organization events configuration error: {e}"));
    if organization_events.is_some()
        && !migration::organization_integration_ready(&state.db)
            .await
            .expect("Organization integration migration readiness")
    {
        panic!("Organization integration migration is required (organization-integration-migration up)");
    }
    let events_router = organization_events
        .map(|verifier| backend::services::organization_events::router(state.clone(), verifier));
    if events_router.is_some() {
        tracing::info!("Organizations event ingress enabled");
        // Offboarding / restoration of organization-scope principals (needs the signed
        // Platform Access client of the organization relay).
        match state.meeting.core.organization_calling.clone() {
            Some(port) => {
                backend::services::organization_access::spawn(
                    state.db.clone(),
                    port.clone(),
                    std::time::Duration::from_secs(5),
                );
                tracing::info!("organization access worker started");
                // Team chats follow team membership (team.* events mark them due).
                match state
                    .meeting
                    .core
                    .organization_authority
                    .clone()
                    .filter(|a| a.is_authoritative())
                {
                    Some(authority) => {
                        backend::services::team_channels::spawn(
                            state.db.clone(),
                            port,
                            authority,
                            std::time::Duration::from_secs(5),
                        );
                        tracing::info!("team chat worker started");
                    }
                    None => tracing::info!(
                        "team chat worker not started: Organizations is not authoritative"
                    ),
                }
            }
            None => tracing::warn!(
                "organization events are enabled but the organization relay is not configured: \
                 membership changes are recorded and applied once it is"
            ),
        }
    }

    // Build application with routes
    // D1: Swagger UI and the served OpenAPI documents only outside production
    // (`API_DOCS_ENABLED`, see `swagger::docs_enabled`).
    let docs = swagger::docs_enabled();
    let mut app = Router::new().merge(routes::create_routes(state));
    if docs {
        app = app.merge(swagger::swagger_router(&addr));
    }
    let mut app = app
        .layer(cors)
        .layer(tower_http::trace::TraceLayer::new_for_http());
    if let Some(bridge) = bridge_router {
        app = app.merge(bridge);
    }
    if let Some(userinfo) = userinfo_router {
        app = app.merge(userinfo);
    }
    if let Some(events) = events_router {
        app = app.merge(events);
    }
    if let Some(operator) = operator_router {
        app = app.merge(operator);
    }
    if let Some(owner) = owner_router {
        app = app.merge(owner);
    }
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    tracing::info!("🚀 Server running on http://{}", addr);
    if docs {
        tracing::info!("📚 Swagger UI: http://{}/swagger-ui", addr);
        tracing::info!("📖 API Docs: http://{}/api-docs/openapi.json", addr);
    } else {
        tracing::info!("API documentation is not served (production; see API_DOCS_ENABLED)");
    }

    axum::serve(listener, app).await?;

    Ok(())
}
