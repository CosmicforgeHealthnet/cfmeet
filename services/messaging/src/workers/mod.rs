//! Periodic maintenance owned by Messaging (each pass bounded and idempotent; safe on
//! several instances): expired realtime tickets, history transfers, expired KeyPackages
//! and low-stock device alerts. Retention, purge and attachment
//! lifecycle workers are added by their modules.

use crate::state::AppState;

pub async fn run_maintenance(state: AppState, interval: std::time::Duration) {
    loop {
        if let Err(e) = crate::realtime::sessions::cleanup_expired(&state).await {
            tracing::warn!(error = %e, "realtime maintenance pass failed");
        }
        if let Err(e) = crate::devices::history_transfer::expire_due(&state).await {
            tracing::warn!(error = %e, "history-transfer expiry pass failed");
        }
        if let Err(e) = crate::devices::key_package_maintenance(&state).await {
            tracing::warn!(error = %e, "device stock maintenance pass failed");
        }
        tokio::time::sleep(interval).await;
    }
}
