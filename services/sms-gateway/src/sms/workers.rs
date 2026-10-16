//! Background work of the SMS Gateway (every instance; leases and `SKIP LOCKED` make the
//! passes multi-instance safe). One pass ([`sweep`]) is deterministic for tests:
//! rate-window cleanup, content retention and quarantine purge.

use std::time::Duration;

use chrono::Duration as ChronoDuration;

use super::SmsResult;
use crate::{db, state::AppState};

/// What one pass did (counts only).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SweepReport {
    pub rate_windows_purged: u64,
    /// Transports whose sealed content passed its retention (content removed; the
    /// transport facts, masked remote and fingerprints stay).
    pub contents_purged: u64,
    /// Quarantined inbound messages purged.
    pub quarantined_purged: u64,
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
    // Content retention: only messages that can no longer be submitted (final or UNKNOWN —
    // an UNKNOWN message is never resubmitted, so its content is not needed for sending).
    let contents_purged = db::exec(
        &state.db,
        "UPDATE sms_transports SET content_sealed = NULL, content_purged_at = $1, version = version + 1, \
           updated_at = GREATEST(updated_at, $1) \
         WHERE id IN (SELECT id FROM sms_transports WHERE content_sealed IS NOT NULL AND created_at < $2 \
                        AND status IN ('DELIVERED', 'RECEIVED', 'FAILED', 'EXPIRED', 'UNKNOWN') \
                      ORDER BY created_at LIMIT 500 FOR UPDATE SKIP LOCKED)",
        vec![now.into(), (now - state.transport.content_retention).into()],
    )
    .await?;
    let quarantined_purged = db::exec(
        &state.db,
        "UPDATE sms_quarantined_inbound SET content_sealed = NULL, purged_at = $1 \
         WHERE id IN (SELECT id FROM sms_quarantined_inbound WHERE purged_at IS NULL AND purge_after <= $1 \
                      ORDER BY purge_after LIMIT 500 FOR UPDATE SKIP LOCKED)",
        vec![now.into()],
    )
    .await?;
    Ok(SweepReport {
        rate_windows_purged,
        contents_purged,
        quarantined_purged,
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
