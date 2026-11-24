//! Periodic maintenance owned by Messaging (each pass bounded and idempotent; safe on
//! several instances): expired realtime tickets, history transfers, expired KeyPackages,
//! low-stock device alerts and the retention of finished push jobs (and of settled
//! notification publications). Retention, purge and
//! attachment lifecycle workers are added by their modules.

use crate::state::AppState;

/// Push-job retention runs at most this often: the purge has no dedicated index (adding
/// one would be a migration the previous image refuses at readiness, breaking rollback),
/// and finished jobs only need to go after 30 days.
const PUSH_RETENTION_EVERY: std::time::Duration = std::time::Duration::from_secs(3600);

pub async fn run_maintenance(state: AppState, interval: std::time::Duration) {
    let mut last_push_retention: Option<std::time::Instant> = None;
    loop {
        if let Err(e) = crate::realtime::sessions::cleanup_expired(&state).await {
            tracing::warn!(error = %e, "realtime maintenance pass failed");
        }
        if let Err(e) = crate::devices::history_transfer::expire_due(&state).await {
            tracing::warn!(error = %e, "history-transfer expiry pass failed");
        }
        if let Err(e) = crate::devices::history_transfer_v2::expire_due(&state).await {
            tracing::warn!(error = %e, "history-transfer v2 expiry pass failed");
        }
        if let Err(e) = crate::devices::key_package_maintenance(&state).await {
            tracing::warn!(error = %e, "device stock maintenance pass failed");
        }
        // Gap audit S8: PRIVATE_E2EE groups still waiting, after 15 minutes, for a
        // member's commit that removes a removed member's devices (they accept only
        // commits meanwhile), so an alert can fire instead of the group staying silent.
        match crate::db::scalar_i64(
            &state.db,
            "SELECT count(DISTINCT conversation_id) FROM messaging_mls_pending_removals \
              WHERE created_at < now() - interval '15 minutes'",
            vec![],
        )
        .await
        {
            Ok(n) => crate::metrics::gauge("messaging_mls_rekey_stalled_conversations", n),
            Err(e) => tracing::warn!(error = %e, "MLS re-key backlog check failed"),
        }
        if last_push_retention.is_none_or(|t| t.elapsed() >= PUSH_RETENTION_EVERY) {
            last_push_retention = Some(std::time::Instant::now());
            if let Err(e) =
                crate::realtime::push::purge_completed(&state, crate::realtime::push::PURGE_BATCH)
                    .await
            {
                tracing::warn!(error = %e, "push job retention pass failed");
            }
            // Gap audit S5: published events past their retention.
            match crate::events::outbox::prune_published(
                &state.db,
                state.now(),
                crate::events::outbox::event_retention_days(),
                crate::realtime::push::PURGE_BATCH,
            )
            .await
            {
                Ok(0) => {}
                Ok(n) => tracing::info!(pruned = n, "conversation events past retention pruned"),
                Err(e) => tracing::warn!(error = %e, "event retention pass failed"),
            }
            // Notification Service N7: overdue and settled routing publications.
            if let Err(e) = crate::notification_publications::maintain(
                &state,
                crate::realtime::push::PURGE_BATCH,
            )
            .await
            {
                tracing::warn!(error = %e, "notification publication maintenance pass failed");
            }
            if let Err(e) =
                crate::member_notifications::maintain(&state, crate::realtime::push::PURGE_BATCH)
                    .await
            {
                tracing::warn!(error = %e, "member notification maintenance pass failed");
            }
        }
        tokio::time::sleep(interval).await;
    }
}
