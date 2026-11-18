//! Notification Service entrypoint (composition root). The API and the workers run in the
//! same process; two replicas coordinate only through the database.

use std::sync::Arc;

use cf_notification_client::seal::OpeningKey;
use cf_push_transport::{adapters::PushAdapters, LocalTestPush, PushProvider};
use notifications::{
    authorities::{Authorities, HttpAuthorities, NoAuthorities},
    config::{NotificationsConfig, PartnerTransportKind, PushProviderKind},
    crypto::DataKeys,
    email, http,
    metrics::{Heartbeats, Metrics},
    preference_copy::HubPreferenceSource,
    state::AppState,
    workers,
};
use secrecy::SecretString;

fn fatal(what: &str, err: impl std::fmt::Display) -> ! {
    eprintln!("notifications: {what}: {err}");
    std::process::exit(2);
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let config =
        NotificationsConfig::from_env().unwrap_or_else(|e| fatal("configuration error", e));
    let check_only = std::env::args().any(|a| a == "--check-config");
    if !check_only {
        notifications::init_tracing(config.log_format);
    }
    let clock = Arc::new(cf_service_auth::SystemClock);
    let verifier = config
        .verifier(clock.clone())
        .unwrap_or_else(|e| fatal("service authentication configuration error", e));
    let keys = DataKeys::new(
        (&config.data_key.0, &config.data_key.1),
        config
            .data_key_previous
            .as_ref()
            .map(|(k, v)| (k.as_str(), v.as_slice())),
    )
    .unwrap_or_else(|e| fatal("data key error", e));
    let opening = config
        .hpke
        .as_ref()
        .map(|(kid, raw)| OpeningKey::from_bytes(kid, raw).map(Arc::new))
        .transpose()
        .unwrap_or_else(|e| fatal("HPKE key error", e));
    let email_provider = email::provider_from_config(&config.email)
        .unwrap_or_else(|e| fatal("email provider error", e));
    let push: Option<Arc<dyn PushProvider>> = match config.push_provider {
        PushProviderKind::None => None,
        PushProviderKind::LocalTest => Some(Arc::new(LocalTestPush::default())),
        PushProviderKind::Live => {
            let adapters = PushAdapters::build(&config.push, false, None)
                .unwrap_or_else(|e| fatal("push adapter error", e));
            if adapters.configured().is_empty() {
                None
            } else {
                Some(Arc::new(adapters))
            }
        }
    };
    // ADR 0063: partner push only while NOTIFICATIONS_PARTNER_PUSH_ENABLED is on.
    let partner = config.partner_push_enabled.then(|| {
        let transport: Arc<dyn notifications::partner::PartnerTransport> = match config
            .partner_push_transport
        {
            PartnerTransportKind::Live => Arc::new(notifications::partner::LivePartnerTransport),
            PartnerTransportKind::LocalTest => {
                Arc::new(notifications::partner::RecordingPartnerTransport::default())
            }
        };
        Arc::new(notifications::partner::PartnerPush::new(transport))
    });
    let http_authorities = match HttpAuthorities::from_config(&config, clock.clone()) {
        Ok(a) => a.map(Arc::new),
        Err(e) => fatal("owner API configuration error", e),
    };
    // Roadmap N10: the preference copy reads the Hub through the same signed client.
    let hub_preferences: Option<Arc<dyn HubPreferenceSource>> = http_authorities
        .as_ref()
        .filter(|a| a.hub_configured())
        .map(|a| a.clone() as Arc<dyn HubPreferenceSource>);
    let authorities: Arc<dyn Authorities> = match http_authorities {
        Some(a) => a,
        None => Arc::new(NoAuthorities),
    };
    if check_only {
        println!(
            "{}",
            serde_json::json!({
                "service": "notifications",
                "configuration": "valid",
                "runtimeEnvironment": config.runtime_environment.as_str(),
                "bindAddress": config.bind_address.to_string(),
                "emailProvider": config.email.provider.as_str(),
                "emailFrom": config.email.from,
                "pushPlatforms": push.as_ref().map(|p| ["APNS", "FCM", "WEB_PUSH"].iter().filter(|x| p.supports(x)).count()).unwrap_or(0),
                // Public by nature: must equal Messaging's `GET /v1/messaging/push/configuration`
                // key, or copied browser subscriptions cannot be delivered (N9, D-1b).
                "webPushApplicationServerKey": push.as_ref().and_then(|p| p.web_push_application_server_key()),
                "authorities": authorities.configured(),
                "sealedOpening": opening.is_some(),
                "partnerPush": partner.is_some(),
                "workers": config.workers_enabled,
            })
        );
        return Ok(());
    }
    tracing::info!(
        runtime_environment = %config.runtime_environment,
        replica = %config.replica_id,
        email_provider = config.email.provider.as_str(),
        push = push.is_some(),
        authorities = ?authorities.configured(),
        "notifications configuration loaded"
    );
    let db = notifications::connect(config.database_url(), config.database_max_connections).await?;
    let state = AppState {
        db,
        verifier,
        keys: Arc::new(keys),
        opening,
        email: Arc::new(config.email.clone()),
        email_provider,
        push,
        authorities,
        metrics: Arc::new(Metrics::default()),
        heartbeats: Arc::new(Heartbeats::default()),
        replica_id: config.replica_id.clone(),
        runtime_environment: config.runtime_environment,
        workers_enabled: config.workers_enabled,
        operator_emails: config.operator_emails.clone(),
        operator_alert_token: config
            .operator_alert_token
            .clone()
            .map(|t: SecretString| Arc::new(t)),
        hub_preferences,
        first_party_scope: config
            .authorities
            .first_party_scope
            .map(|s| (s.tenant_id, s.application_id, s.environment_id)),
        partner,
    };
    let running = config.workers_enabled.then(|| workers::spawn(&state));
    let listener = tokio::net::TcpListener::bind(config.bind_address).await?;
    tracing::info!(bind = %config.bind_address, "notifications listening");
    axum::serve(listener, http::app(state))
        .with_graceful_shutdown(notifications::shutdown_signal())
        .await?;
    if let Some(w) = running {
        w.shutdown();
    }
    Ok(())
}
