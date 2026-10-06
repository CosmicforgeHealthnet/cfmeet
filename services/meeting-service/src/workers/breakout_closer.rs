//! Breakout countdown worker (Part F §39).
//!
//! CLOSING rooms carry a persisted `closing_at` deadline, so a restart never loses a
//! countdown: any instance finalizes due rooms (CLOSING → CLOSED, participants back to
//! main) under the conference row lock. Finalization is idempotent, so several
//! instances — or a manual immediate close racing the worker — are safe.

use tokio::sync::watch;

use crate::services::{collaboration::breakouts, meeting_core::Core};

pub struct BreakoutCloser {
    core: Core,
}

impl BreakoutCloser {
    pub fn new(core: Core) -> Self {
        Self { core }
    }

    /// One pass. Returns how many rooms were closed.
    pub async fn tick(&self) -> crate::error::ApiResult<u32> {
        breakouts::close_due(&self.core, 100).await
    }
}

pub fn spawn_breakout_closer(closer: BreakoutCloser, poll_secs: u64) -> watch::Sender<bool> {
    let (tx, mut rx) = watch::channel(false);
    tokio::spawn(async move {
        loop {
            if *rx.borrow() {
                break;
            }
            match closer.tick().await {
                Ok(n) if n > 0 => tracing::info!(closed = n, "breakout countdowns completed"),
                Ok(_) => {}
                Err(e) => tracing::error!("breakout closer pass failed: {e}"),
            }
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(poll_secs)) => {}
                _ = rx.changed() => {}
            }
        }
    });
    tx
}
