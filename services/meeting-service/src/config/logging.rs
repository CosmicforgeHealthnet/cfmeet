use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

pub fn init_logging() -> WorkerGuard {
    // 1. Determine environment
    let env = std::env::var("APP_ENV").unwrap_or_else(|_| "dev".to_string());
    let log_dir = std::env::var("CFMEET_LOG_DIRECTORY").unwrap_or_else(|_| {
        if env == "prod" || env == "production" {
            "logs/prod".to_string()
        } else {
            "target/logs/dev".to_string()
        }
    });

    // 2. Setup File Appender (Daily Rotation)
    // Fifteen daily files keep the previous fourteen days plus today's log. Production
    // mounts this directory from the host, so replacing a container cannot erase it.
    let daily_appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("backend.log")
        .max_log_files(15)
        .build(log_dir)
        .expect("meeting-service log directory must be writable");
    let (file_writer, guard) = tracing_appender::non_blocking(daily_appender);

    // 3. Terminal Layer (Pretty printing for developers)
    let stdout_layer = fmt::layer()
        .pretty()
        .with_thread_ids(true)
        .with_target(true);

    // 4. File Layer (Compact format - JSON causes issues with tower_http spans)
    let file_layer = fmt::layer()
        .with_writer(file_writer)
        .with_ansi(false)
        .compact()
        .with_target(true);

    // 5. Build and Initialize the Subscriber
    tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "backend=info,tower_http=info".into()),
        )
        .with(stdout_layer)
        .with(file_layer)
        .init();

    guard
}
