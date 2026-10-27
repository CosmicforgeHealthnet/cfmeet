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
        .max_connections(10)
        .min_connections(2)
        .connect_timeout(std::time::Duration::from_secs(30))
        .acquire_timeout(std::time::Duration::from_secs(30))
        .idle_timeout(std::time::Duration::from_secs(300))
        .max_lifetime(std::time::Duration::from_secs(1800))
        .sqlx_logging(false);
    let db = Database::connect(db_opts).await?;
    tracing::info!("✅ Database connected successfully");

    // Load email configuration
    let email_config = match EmailConfig::from_env() {
        Ok(config) => {
            tracing::info!("✅ Email configuration loaded");
            config
        }
        Err(e) => {
            tracing::warn!(
                "⚠️ Email not configured: {}. Using defaults (emails will queue but not send).",
                e
            );
            EmailConfig::default()
        }
    };

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
    tracing::info!("✅ Hosted UI URL: {}", app_config.app_url);

    // Start email worker (only if SMTP is configured)
    let _email_worker = if !email_config.smtp_host.is_empty() {
        match spawn_email_worker(state.email_service.queue().clone(), &email_config) {
            Some(handle) => {
                tracing::info!("✅ Email worker started");
                Some(handle)
            }
            None => {
                tracing::warn!("⚠️ Failed to start email worker");
                None
            }
        }
    } else {
        tracing::info!("ℹ️ Email worker not started (SMTP not configured)");
        None
    };

    // Start meeting auto-end worker (always runs)
    let _meeting_auto_end_worker = spawn_meeting_auto_end_worker(db.clone());
    tracing::info!("✅ Meeting auto-end worker started");

    // Start meeting auto-start worker (always runs)
    let _meeting_auto_start_worker = spawn_meeting_auto_start_worker(db.clone());
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

    // Build application with routes
    let mut app = Router::new()
        .merge(routes::create_routes(state))
        .merge(swagger::swagger_router(&addr))
        .layer(cors)
        .layer(tower_http::trace::TraceLayer::new_for_http());
    if let Some(bridge) = bridge_router {
        app = app.merge(bridge);
    }
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    tracing::info!("🚀 Server running on http://{}", addr);
    tracing::info!("📚 Swagger UI: http://{}/swagger-ui", addr);
    tracing::info!("📖 API Docs: http://{}/api-docs/openapi.json", addr);

    axum::serve(listener, app).await?;

    Ok(())
}
