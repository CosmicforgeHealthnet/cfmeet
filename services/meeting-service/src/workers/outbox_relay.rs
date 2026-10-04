//! Outbox relay — delivers committed `outbox_events` (spec §B13, §3.5, Part E §11–§14).
//! Events are delivery, never authority: a consumer recovers canonical state from the
//! API/database.
//!
//! Two logically distinct channels:
//!
//! - `REALTIME` client events → an [`EventPublisher`] (Redis fanout in production);
//! - `PROVIDER_COMMAND` rows → the RTC control plane ([`RtcControl`]), which re-reads
//!   authoritative state and enforces it at the provider. Commands are never handed to
//!   a client-facing publisher.
//!
//! Rows are claimed with crash-safe leases (`FOR UPDATE SKIP LOCKED` + `available_at`
//! lease), so the relay is retry-, crash- and multi-worker-safe. Transient failures
//! (Redis / provider outage) retry with capped backoff and are never dropped; poison
//! rows (malformed, unknown, unpublishable) are dead-lettered as FAILED and counted.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::DatabaseConnection;
use tokio::sync::watch;

use crate::{
    domain::OutboxChannel,
    error::ApiResult,
    models::outbox_events,
    services::{
        outbox,
        rtc_control::{CommandResult, RtcControl},
        rtc_metrics::{inc, RTC_METRICS as M},
    },
};

/// Seconds a claimed row stays invisible to other relays.
const LEASE_SECS: i64 = 30;

#[async_trait]
pub trait EventPublisher: Send + Sync {
    async fn publish(&self, event: &outbox_events::Model) -> Result<(), String>;

    /// Whether this sink may also receive provider commands. Only non-client sinks
    /// (development log) may; client-facing publishers never do.
    fn accepts_commands(&self) -> bool {
        false
    }
}

/// Development publisher: structured log only (never a client channel).
pub struct LogPublisher;

#[async_trait]
impl EventPublisher for LogPublisher {
    async fn publish(&self, event: &outbox_events::Model) -> Result<(), String> {
        tracing::info!(event_type = %event.event_type, aggregate_id = %event.aggregate_id, "outbox event");
        Ok(())
    }

    fn accepts_commands(&self) -> bool {
        true
    }
}

pub struct OutboxRelay {
    db: DatabaseConnection,
    publisher: Option<Arc<dyn EventPublisher>>,
    control: Option<Arc<RtcControl>>,
    holder: String,
}

impl OutboxRelay {
    /// Delivery-only relay: every channel goes to `publisher` when it accepts it.
    pub fn new(db: DatabaseConnection, publisher: Arc<dyn EventPublisher>) -> Self {
        Self {
            db,
            publisher: Some(publisher),
            control: None,
            holder: outbox::holder_id("relay"),
        }
    }

    /// Part E relay: realtime events → `publisher` (when realtime is enabled),
    /// provider commands → `control` (when provider control is active).
    pub fn with_control(
        db: DatabaseConnection,
        publisher: Option<Arc<dyn EventPublisher>>,
        control: Option<Arc<RtcControl>>,
    ) -> Self {
        Self {
            db,
            publisher,
            control,
            holder: outbox::holder_id("relay"),
        }
    }

    /// Deliver one batch per channel. Returns (published, failed_attempts).
    pub async fn tick(&self, batch: u64) -> ApiResult<(u64, u64)> {
        let (mut ok, mut failed) = (0, 0);
        if let Some(publisher) = &self.publisher {
            let rows = outbox::claim(
                &self.db,
                OutboxChannel::Realtime,
                None,
                batch,
                LEASE_SECS,
                &self.holder,
            )
            .await?;
            for row in rows {
                if self.deliver(publisher.as_ref(), &row).await? {
                    ok += 1;
                } else {
                    failed += 1;
                }
            }
        }
        match (&self.control, &self.publisher) {
            (Some(control), _) if control.room_control() => {
                let rows = outbox::claim(
                    &self.db,
                    OutboxChannel::ProviderCommand,
                    None,
                    batch,
                    LEASE_SECS,
                    &self.holder,
                )
                .await?;
                for row in rows {
                    match control.process(&row, &self.holder).await {
                        CommandResult::Done => ok += 1,
                        _ => failed += 1,
                    }
                }
            }
            (None, Some(publisher)) if publisher.accepts_commands() => {
                let rows = outbox::claim(
                    &self.db,
                    OutboxChannel::ProviderCommand,
                    None,
                    batch,
                    LEASE_SECS,
                    &self.holder,
                )
                .await?;
                for row in rows {
                    if self.deliver(publisher.as_ref(), &row).await? {
                        ok += 1;
                    } else {
                        failed += 1;
                    }
                }
            }
            // No consumer: provider commands stay durably PENDING.
            _ => {}
        }
        Ok((ok, failed))
    }

    async fn deliver(&self, publisher: &dyn EventPublisher, row: &outbox_events::Model) -> ApiResult<bool> {
        if row.channel == OutboxChannel::Realtime
            && crate::services::realtime::envelope(row).is_none()
        {
            inc(&M.outbox_dead_lettered);
            outbox::mark_failed(&self.db, row.id, &self.holder, "unpublishable event").await?;
            return Ok(false);
        }
        match publisher.publish(row).await {
            Ok(()) => {
                inc(&M.outbox_published);
                outbox::mark_published(&self.db, row.id, &self.holder).await?;
                Ok(true)
            }
            Err(e) => {
                // Delivery infrastructure outage (e.g. Redis): retried, never dropped.
                inc(&M.outbox_retried);
                outbox::mark_retry(&self.db, row, &self.holder, &e, true).await?;
                Ok(false)
            }
        }
    }
}

pub fn spawn_outbox_relay(relay: OutboxRelay, poll_secs: u64) -> watch::Sender<bool> {
    let (tx, mut rx) = watch::channel(false);
    tokio::spawn(async move {
        loop {
            if *rx.borrow() {
                break;
            }
            match relay.tick(100).await {
                Ok((published, failed)) if published + failed > 0 => {
                    tracing::debug!(published, failed, "outbox relay tick");
                }
                Ok(_) => {}
                Err(e) => tracing::error!("outbox relay tick failed: {e}"),
            }
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(poll_secs)) => {}
                _ = rx.changed() => {}
            }
        }
    });
    tx
}
