//! Structured logging. Output goes to stdout; the filter comes from `RUST_LOG`.
//! Nothing in this service logs credential secrets, verifiers, `Authorization` headers or
//! contact data.

use std::io::IsTerminal;

use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::config::LogFormat;

pub const DEFAULT_FILTER: &str = "calling=info,platform_admin=info,warn";

pub fn init(format: LogFormat) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| DEFAULT_FILTER.into());
    let registry = tracing_subscriber::registry().with(filter);
    match format {
        LogFormat::Json => registry
            .with(
                fmt::layer()
                    .json()
                    .with_current_span(true)
                    .with_span_list(false),
            )
            .init(),
        LogFormat::Pretty => registry
            .with(
                fmt::layer()
                    .with_target(true)
                    .with_ansi(std::io::stdout().is_terminal()),
            )
            .init(),
    }
}

/// Operator CLI logging: human-readable, to stderr (stdout carries command output).
pub fn init_cli() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "platform_admin=info,calling=info,warn".into());
    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .with_writer(std::io::stderr)
                .with_target(false)
                .with_ansi(std::io::stderr().is_terminal()),
        )
        .init();
}
