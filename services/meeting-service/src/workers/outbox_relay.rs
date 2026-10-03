//! Outbox relay — delivers committed `outbox_events` to an [`EventPublisher`]
//! (spec §B13, §3.5). Events are delivery, never authority: a consumer recovers
//! canonical state from the API/database.
//!
//! Claims rows with `FOR UPDATE SKIP LOCKED` (multi-instance safe), retries with
//! backoff and marks FAILED after `MAX_ATTEMPTS`. Only started when the Part E
//! realtime/control-plane consumer is enabled (`conference_realtime_enabled`), so
//! provider intents are never "delivered" to nowhere.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use sea_orm::{
    sea_query::{LockBehavior, LockType},
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
};
use tokio::sync::watch;

use crate::{domain::OutboxStatus, error::ApiResult, models::outbox_events};

const MAX_ATTEMPTS: i32 = 10;

#[async_trait]
pub trait EventPublisher: Send + Sync {
    async fn publish(&self, event: &outbox_events::Model) -> Result<(), String>;
}

/// Development publisher: structured log only.
pub struct LogPublisher;

#[async_trait]
impl EventPublisher for LogPublisher {
    async fn publish(&self, event: &outbox_events::Model) -> Result<(), String> {
        tracing::info!(event_type = %event.event_type, aggregate_id = %event.aggregate_id, "outbox event");
        Ok(())
    }
}

pub struct OutboxRelay {
    db: DatabaseConnection,
    publisher: Arc<dyn EventPublisher>,
}

impl OutboxRelay {
    pub fn new(db: DatabaseConnection, publisher: Arc<dyn EventPublisher>) -> Self {
        Self { db, publisher }
    }

    /// Deliver one batch. Returns (published, failed_attempts).
    pub async fn tick(&self, batch: u64) -> ApiResult<(u64, u64)> {
        let txn = self.db.begin().await?;
        let now = Utc::now();
        let events = outbox_events::Entity::find()
            .filter(outbox_events::Column::Status.eq(OutboxStatus::Pending))
            .filter(outbox_events::Column::AvailableAt.lte(now))
            .order_by_asc(outbox_events::Column::CreatedAt)
            .limit(batch)
            .lock_with_behavior(LockType::Update, LockBehavior::SkipLocked)
            .all(&txn)
            .await?;
        let (mut ok, mut failed) = (0, 0);
        for e in events {
            let attempts = e.attempts + 1;
            let mut am: outbox_events::ActiveModel = e.clone().into();
            am.attempts = Set(attempts);
            match self.publisher.publish(&e).await {
                Ok(()) => {
                    am.status = Set(OutboxStatus::Published);
                    am.published_at = Set(Some(Utc::now()));
                    am.last_error = Set(None);
                    ok += 1;
                }
                Err(err) => {
                    failed += 1;
                    am.last_error = Set(Some(err.chars().take(500).collect()));
                    if attempts >= MAX_ATTEMPTS {
                        am.status = Set(OutboxStatus::Failed);
                    } else {
                        am.available_at =
                            Set(now + Duration::seconds(2i64.pow(attempts.min(10) as u32)));
                    }
                }
            }
            am.update(&txn).await?;
        }
        txn.commit().await?;
        Ok((ok, failed))
    }
}

pub fn spawn_outbox_relay(relay: OutboxRelay, poll_secs: u64) -> watch::Sender<bool> {
    let (tx, mut rx) = watch::channel(false);
    tokio::spawn(async move {
        loop {
            if *rx.borrow() {
                break;
            }
            if let Err(e) = relay.tick(100).await {
                tracing::error!("outbox relay tick failed: {e}");
            }
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(poll_secs)) => {}
                _ = rx.changed() => {}
            }
        }
    });
    tx
}
