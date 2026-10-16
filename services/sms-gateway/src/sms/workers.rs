//! Background work of the SMS Gateway (every instance; leases and `SKIP LOCKED` make the
//! passes multi-instance safe). One pass ([`sweep`]) is deterministic for tests.

use std::time::Duration;

use chrono::Duration as ChronoDuration;

use super::SmsResult;
use crate::{db, state::AppState};

/// What one pass did (counts only).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SweepReport {
    pub rate_windows_purged: u64,
}

/// One pass of every periodic task.
pub async fn sweep(state: &AppState) -> SmsResult<SweepReport> {
    let now = state.now();
    let rate_windows_purged = db::exec(
        &state.db,
        "DELETE FROM sms_rate_windows WHERE window_start < $1",
        vec![(now - ChronoDuration::days(2)).into()],
    )
    .await?;
    Ok(SweepReport {
        rate_windows_purged,
    })
}

/// Runs [`sweep`] every worker interval until the process stops.
pub async fn run(state: AppState) {
    let interval: Duration = state.workers.interval;
    loop {
        if let Err(e) = sweep(&state).await {
            tracing::warn!(error = %e, "sms worker pass failed");
        }
        tokio::time::sleep(interval).await;
    }
}
