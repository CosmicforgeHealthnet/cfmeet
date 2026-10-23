//! Developer Platform worker entrypoint: its own background loop, separate from the API
//! process (independently scalable and restartable).

use developer_platform::config::{service_auth::ServiceAuthSettings, DeveloperPlatformConfig};

fn fatal(what: &str, err: impl std::fmt::Display) -> ! {
    eprintln!("developer-platform-worker: {what}: {err}");
    std::process::exit(2);
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let config =
        DeveloperPlatformConfig::from_env().unwrap_or_else(|e| fatal("configuration error", e));
    // The worker signs as the Developer Platform too: validate the same identity inputs.
    let settings = ServiceAuthSettings::from_env()
        .unwrap_or_else(|e| fatal("service authentication configuration error", e));
    developer_platform::init_tracing(config.log_format);
    let runtime = settings.runtime_environment.as_str().to_string();
    let db =
        developer_platform::connect(config.database_url(), config.database_max_connections).await?;
    tracing::info!(instance = %config.worker_instance_id, runtime_environment = %runtime,
        interval_ms = config.worker_interval.as_millis() as u64, "developer platform worker started");
    developer_platform::worker::run(
        db,
        config.worker_instance_id,
        runtime,
        config.worker_interval,
        async {
            let _ = tokio::signal::ctrl_c().await;
        },
    )
    .await;
    Ok(())
}
