//! Compliance (Part V Part G, §89–§96): legal [`holds`], [`retention`] and the
//! irreversible purge, authorized MANAGED-only [`exports`], data [`deletion`] requests
//! and the administrative [`audit_log`] API. Every purge or deletion asks the holds
//! first; nothing here can decrypt PRIVATE_E2EE content.

pub mod audit_log;
pub mod deletion;
pub mod exports;
pub mod holds;
pub mod http;
pub mod retention;

use crate::state::AppState;

/// One pass of every compliance worker (bounded; safe on several instances).
pub async fn run_once(state: &AppState) -> crate::error::Result<retention::RetentionReport> {
    let report = retention::run_once(state).await?;
    exports::run_pending(state, 5).await?;
    exports::expire(state, 100).await?;
    deletion::run_pending(state, 5).await?;
    Ok(report)
}

pub async fn run(state: AppState, interval: std::time::Duration) {
    loop {
        match run_once(&state).await {
            Ok(r) if r != retention::RetentionReport::default() => {
                tracing::info!(
                    expired = r.expired,
                    purged = r.purged,
                    held = r.held,
                    attachments = r.attachments_deleted,
                    "compliance pass"
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "compliance pass failed"),
        }
        tokio::time::sleep(interval).await;
    }
}
