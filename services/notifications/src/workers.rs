//! The service's workers (Document 4 §11), one set per replica; replicas coordinate only
//! through the database (`FOR UPDATE SKIP LOCKED` claims, renewable leases):
//!
//! | Worker | Lane | Cadence |
//! |---|---|---|
//! | W1 ingest processor | all (CRITICAL claimed first) | 100 ms wake + poll |
//! | W2 critical dispatcher: email CRITICAL lane (X6 lease, X7 completion) | CRITICAL | 100 ms |
//! | W3 standard dispatcher: email STANDARD lane + push | HIGH / STANDARD | 500 ms |
//! | W4 scheduler: reminders after the takeover, expiry of overdue deliveries | — | 5 s |
//! | W5 retention and erasure | — | hourly |
//! | W6 shadow comparator | — | on read (I7 aggregates the observations recorded by W1) |
//! | W7 preference copy + catch-up (roadmap N10; idle while `notif_preference_copy.mode` is OFF) | — | 50 ms – 5 s |
//!
//! With no email provider (`none`) W2 and W3-email are not started: mail stays QUEUED and
//! expires (codes never linger). With no push provider the push part is not started.

use std::{sync::Arc, time::Duration};

use chrono::Utc;
use sea_orm::DatabaseConnection;
use tokio::sync::watch;

use crate::{db, email::EmailWorker, push::PushWorker, state::AppState};

pub struct Workers {
    shutdown: watch::Sender<bool>,
    handles: Vec<tokio::task::JoinHandle<()>>,
}

impl Workers {
    pub fn shutdown(self) {
        let _ = self.shutdown.send(true);
        for h in self.handles {
            h.abort();
        }
    }
}

pub fn spawn(state: &AppState) -> Workers {
    let (tx, rx) = watch::channel(false);
    let mut handles = Vec::new();
    let hb = state.heartbeats.clone();
    {
        let p = state.processor();
        let (hb, rx) = (hb.clone(), rx.clone());
        handles.push(tokio::spawn(async move {
            p.run(Duration::from_millis(100), hb, rx).await
        }));
    }
    if let Some(provider) = &state.email_provider {
        for (lane, poll) in [("CRITICAL", 100), ("STANDARD", 500)] {
            let w = EmailWorker::new(
                state.db.clone(),
                state.keys.clone(),
                provider.clone(),
                state.authorities.clone(),
                state.email.clone(),
                state.metrics.clone(),
                lane,
                &state.replica_id,
            );
            let (hb, rx) = (hb.clone(), rx.clone());
            handles.push(tokio::spawn(async move {
                w.run(Duration::from_millis(poll), hb, rx).await
            }));
        }
    }
    if let Some(push) = &state.push {
        let w = PushWorker::new(
            state.db.clone(),
            state.keys.clone(),
            push.clone(),
            state.metrics.clone(),
        );
        let (hb, rx) = (hb.clone(), rx.clone());
        handles.push(tokio::spawn(async move {
            w.run(Duration::from_millis(500), hb, rx).await
        }));
    }
    {
        let p = state.processor();
        let (hb, mut rx) = (hb.clone(), rx.clone());
        let db = state.db.clone();
        handles.push(tokio::spawn(async move {
            let mut last_retention = Utc::now() - chrono::Duration::hours(2);
            loop {
                if *rx.borrow() {
                    break;
                }
                hb.beat("scheduler");
                if let Err(e) = p.fire_schedules(Utc::now()).await {
                    tracing::warn!(summary = %db::summarize_db_error(&e), "scheduler tick failed");
                }
                if let Err(e) = expire_overdue(&db).await {
                    tracing::warn!(summary = %db::summarize_db_error(&e), "expiry pass failed");
                }
                // Retention runs hourly inside this loop, whose own beat ("scheduler") proves
                // it alive: an hourly heartbeat would read stale to /ready after 60 s.
                if Utc::now() - last_retention > chrono::Duration::hours(1) {
                    match retention(&db).await {
                        Ok(()) => last_retention = Utc::now(),
                        Err(e) => tracing::warn!(summary = %db::summarize_db_error(&e), "retention pass failed"),
                    }
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                    _ = rx.changed() => {}
                }
            }
        }));
    }
    if let Some(hub) = &state.hub_preferences {
        let (db, hub, rx) = (state.db.clone(), hub.clone(), rx.clone());
        handles.push(tokio::spawn(async move {
            crate::preference_copy::run(db, hub, rx).await
        }));
    }
    Workers {
        shutdown: tx,
        handles,
    }
}

/// W4: deliveries whose notification expired are never sent late.
pub async fn expire_overdue(db_conn: &DatabaseConnection) -> Result<(), sea_orm::DbErr> {
    db::exec(
        db_conn,
        "UPDATE notif_deliveries d SET status = 'EXPIRED', completed_at = now()
           FROM notif_notifications n
          WHERE n.id = d.notification_id AND d.status IN ('PENDING', 'DEFERRED') AND d.channel IN ('PUSH', 'IN_APP')
            AND n.expires_at IS NOT NULL AND n.expires_at < now()",
        vec![],
    )
    .await?;
    db::exec(
        db_conn,
        "DELETE FROM notif_service_nonces WHERE expires_at < now()",
        vec![],
    )
    .await?;
    Ok(())
}

/// W5: retention (Document 4 §6 retention column) and the 7-day preference change log.
pub async fn retention(db_conn: &DatabaseConnection) -> Result<(), sea_orm::DbErr> {
    let statements = [
        "DELETE FROM notif_inbox_items WHERE expires_at < now()",
        "DELETE FROM notif_event_inbox WHERE state IN ('PROCESSED', 'REJECTED', 'SHADOWED') AND received_at < now() - interval '30 days'",
        "DELETE FROM notif_notifications n WHERE n.created_at < now() - interval '30 days'
            AND NOT EXISTS (SELECT 1 FROM notif_deliveries d WHERE d.notification_id = n.id AND d.status IN ('PENDING', 'DEFERRED', 'DISPATCHING'))
            AND NOT EXISTS (SELECT 1 FROM notif_inbox_items i WHERE i.id = n.id)",
        "DELETE FROM notif_shadow_observations WHERE observed_at < now() - interval '30 days'",
        "DELETE FROM notif_preference_change_log WHERE changed_at < now() - interval '7 days'",
        "DELETE FROM notif_schedules WHERE state IN ('FIRED', 'CANCELLED') AND created_at < now() - interval '7 days'",
        "DELETE FROM notif_push_endpoints WHERE NOT enabled AND updated_at < now() - interval '30 days'",
    ];
    for sql in statements {
        db::exec(db_conn, sql, vec![]).await?;
    }
    Ok(())
}

/// Erasure of every row for an account (Document 4 §8: within 24 h). Called by the
/// operator erasure flow; delivery history keeps only digests.
pub async fn erase_account(
    db_conn: &DatabaseConnection,
    account_ref: &str,
) -> Result<(), sea_orm::DbErr> {
    for sql in [
        "UPDATE notif_email_messages SET recipient_sealed = NULL, body_sealed = NULL
          WHERE delivery_id IN (SELECT d.id FROM notif_deliveries d JOIN notif_notifications n ON n.id = d.notification_id
                                 WHERE n.recipient_ref = $1)",
        "DELETE FROM notif_inbox_items WHERE recipient_ref = $1",
        "DELETE FROM notif_push_endpoints WHERE account_ref = $1",
        "DELETE FROM notif_preferences WHERE account_ref = $1",
        "DELETE FROM notif_conversation_preferences WHERE account_ref = $1",
        "DELETE FROM notif_contact_preferences WHERE account_ref = $1",
        "DELETE FROM notif_preference_change_log WHERE account_ref = $1",
        "DELETE FROM notif_notifications WHERE recipient_ref = $1",
    ] {
        db::exec(db_conn, sql, vec![account_ref.into()]).await?;
    }
    Ok(())
}

#[allow(dead_code)]
fn _assert_send(state: Arc<AppState>) {
    let _ = state;
}
