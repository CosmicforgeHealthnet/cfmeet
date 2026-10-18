//! Periodic maintenance owned by Messaging (each pass bounded and idempotent; safe on
//! several instances): expired realtime tickets. Retention, purge and attachment
//! lifecycle workers are added by their modules.

use crate::state::AppState;

pub async fn run_maintenance(state: AppState, interval: std::time::Duration) {
    loop {
        if let Err(e) = crate::realtime::sessions::cleanup_expired(&state).await {
            tracing::warn!(error = %e, "realtime maintenance pass failed");
        }
        tokio::time::sleep(interval).await;
    }
}
