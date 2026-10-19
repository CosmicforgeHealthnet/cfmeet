//! Unified Messaging sync worker (Part V H §§98, 100, 104).
//!
//! Each pass ([`UnifiedMessaging::sync_once`]) links live conferences to their MEETING
//! conversation, applies admission / role changes (version-fenced in Messaging), closes
//! conversations of ended conferences, mirrors and validates main-room chat in shadow
//! mode, copies messages back after a rollback and completes durable logout
//! revocations. Every step is idempotent and row-locked, so several instances are safe;
//! a Messaging outage only defers work (it never touches a meeting).

use std::{sync::Arc, time::Duration};

use tokio::sync::watch;

use crate::services::unified_messaging::UnifiedMessaging;

pub fn sync_interval() -> Duration {
    Duration::from_millis(
        crate::config::env_num("UNIFIED_MESSAGING_SYNC_INTERVAL_MS", 2000u64).clamp(200, 60_000),
    )
}

pub fn spawn_unified_messaging_sync(
    um: Arc<UnifiedMessaging>,
    every: Duration,
) -> watch::Sender<bool> {
    let (tx, mut rx) = watch::channel(false);
    tokio::spawn(async move {
        loop {
            if *rx.borrow() {
                break;
            }
            match um.sync_once().await {
                Ok(r) if r.failures > 0 => {
                    tracing::warn!(
                        failures = r.failures,
                        "unified messaging sync pass partly deferred"
                    )
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("unified messaging sync pass failed: {e}"),
            }
            tokio::select! {
                _ = tokio::time::sleep(every) => {}
                _ = rx.changed() => {}
            }
        }
    });
    tx
}
