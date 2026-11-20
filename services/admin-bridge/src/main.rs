//! Admin bridge entrypoint (composition root). Off by default: with
//! `ADMIN_BRIDGE_ENABLED=false` only the loopback ops listener runs and HQ gets nothing.
//! Enabled, the owner event listener runs only when `ADMIN_BRIDGE_EVENTS_BIND_ADDRESS` is set.
//! `--check-config` validates the environment and prints a summary without starting.

use std::sync::Arc;

use admin_bridge::{
    chain::Bridge,
    config::{Config, ProcessEnv},
    routes::HealthProbe,
    server,
    store::{pg::PgStore, MemoryStore, Store},
};

fn fatal(what: &str, err: impl std::fmt::Display) -> ! {
    eprintln!("admin-bridge: {what}: {err}");
    std::process::exit(2);
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    let config = Config::from_env(&ProcessEnv).unwrap_or_else(|e| fatal("configuration error", e));
    let check_only = std::env::args().any(|a| a == "--check-config");
    let acceptor = config.tls.as_ref().map(|t| {
        server::tls_acceptor(&t.cert, &t.key, &t.client_ca)
            .unwrap_or_else(|e| fatal("TLS configuration error", e))
    });
    let ingress = config.events.as_ref().map(|e| {
        let policy = cf_service_auth::TrustPolicy::from_file(
            &e.trust_bundle_file,
            &cf_service_auth::ServiceId::admin_bridge(),
            config.runtime_environment,
        )
        .unwrap_or_else(|err| fatal("CF_SERVICE_TRUST_BUNDLE_FILE", err));
        Arc::new(admin_bridge::events::EventIngress::new(
            cf_service_auth::ServiceTokenVerifier::new(policy, Arc::new(cf_service_auth::SystemClock)),
        ))
    });
    let releases = config.github.as_ref().map(|g| {
        let workflow = admin_bridge::releases::ReleaseWorkflow::new(g);
        if !workflow.usable() {
            fatal(
                "ADMIN_BRIDGE_GITHUB_TOKEN_FILE",
                "unreadable, empty or not a single token",
            );
        }
        Arc::new(workflow)
    });
    if check_only {
        println!(
            "{}",
            serde_json::json!({
                "service": "admin-bridge",
                "configuration": "valid",
                "runtimeEnvironment": config.runtime_environment.as_str(),
                "enabled": config.switches.enabled,
                "writesEnabled": config.switches.writes_enabled,
                "modules": config.switches.modules.iter().map(|m| m.as_str()).collect::<Vec<_>>(),
                "hqBindAddress": config.hq_bind.to_string(),
                "opsBindAddress": config.ops_bind.to_string(),
                "policyRevision": config.security.as_ref().map(|s| s.policy_revision),
                "healthProbes": config.health_urls.keys().map(|o| o.as_str()).collect::<Vec<_>>(),
                "owners": config.outbound.as_ref().map(|o| o.base_urls.keys().map(|k| k.as_str()).collect::<Vec<_>>()).unwrap_or_default(),
                "eventsBindAddress": config.events.as_ref().map(|e| e.bind.to_string()),
                "releaseWorkflow": config.github.as_ref().map(|g| g.repository.clone()),
            })
        );
        return;
    }
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    if config.json_logs {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }

    // Disabled without a database: an empty store keeps the ops listener honest.
    let store: Arc<dyn Store> = match &config.database_url {
        Some(url) => Arc::new(
            PgStore::connect(url, config.database_max_connections)
                .await
                .unwrap_or_else(|e| fatal("database error", e)),
        ),
        None => Arc::new(MemoryStore::new()),
    };
    let mut bridge = Bridge::new(
        config.switches.clone(),
        config.security.clone(),
        store,
        Arc::new(chrono::Utc::now),
        HealthProbe::new(config.health_urls.clone()),
    );
    bridge.database_configured = config.database_url.is_some();
    if let Some(outbound) = &config.outbound {
        let owners = admin_bridge::owners::http_owners(outbound, config.runtime_environment)
            .unwrap_or_else(|e| fatal("owner client configuration error", e));
        bridge = bridge.with_owners(Arc::new(owners));
    }
    if let Some(releases) = releases {
        bridge = bridge.with_releases(releases);
    }
    let bridge = Arc::new(bridge);
    tracing::info!(
        runtime_environment = %config.runtime_environment,
        enabled = config.switches.enabled,
        writes_enabled = config.switches.writes_enabled,
        openapi_sha256 = %bridge.openapi_sha256,
        "admin bridge configuration loaded"
    );
    if config.switches.enabled {
        match bridge.load_policy().await {
            Ok(rev) => tracing::info!(policy_revision = rev, "access policy verified"),
            Err(e) => {
                tracing::error!(error = %e, "access policy not verified: every HQ request answers POLICY_UNAVAILABLE")
            }
        }
    }

    let ops = tokio::net::TcpListener::bind(config.ops_bind)
        .await
        .unwrap_or_else(|e| fatal("ops listener", e));
    tokio::spawn(server::run_ops(bridge.clone(), ops));
    tokio::spawn(server::run_maintenance(bridge.clone()));
    if let Some(acceptor) = acceptor {
        let hq = tokio::net::TcpListener::bind(config.hq_bind)
            .await
            .unwrap_or_else(|e| fatal("HQ listener", e));
        tracing::info!(address = %config.hq_bind, "HQ mTLS listener started");
        tokio::spawn(server::run_hq(bridge.clone(), hq, acceptor));
    } else {
        tracing::info!("ADMIN_BRIDGE_ENABLED is off: no HQ listener");
    }
    if let (Some(events), Some(ingress)) = (&config.events, ingress) {
        let listener = tokio::net::TcpListener::bind(events.bind)
            .await
            .unwrap_or_else(|e| fatal("owner event listener", e));
        tracing::info!(address = %events.bind, "owner event listener started");
        tokio::spawn(admin_bridge::events::run_events(
            bridge.clone(),
            listener,
            ingress,
        ));
    }
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
