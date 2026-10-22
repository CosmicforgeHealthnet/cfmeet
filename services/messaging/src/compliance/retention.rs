//! Retention and the irreversible purge (§89–§90).
//!
//! * Retention: per environment, `retention.messageDays` (or a context class's
//!   `retentionDays`) expires ACTIVE messages older than the limit — a tombstone
//!   (`EXPIRED`, durable event) due for purge at once.
//! * Purge: every tombstoned message whose `purge_after` passed loses its stored
//!   ciphertext / sealed text / search entry for good (`PURGED`, a database trigger makes
//!   it irreversible) and its attachments are deleted (objects through the deletion
//!   queue) — UNLESS an active legal hold protects it: then nothing is removed and it is
//!   retried later. Holds therefore suspend purges; they never stop a tombstone from
//!   hiding content from members.
//! * Bounded batches, `FOR UPDATE SKIP LOCKED`, safe on several instances, idempotent.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use uuid::Uuid;

use crate::{
    attachments::{lifecycle::delete_attachment, AttachmentRow, ATTACHMENT_COLUMNS},
    compliance::holds,
    conversations::{policy, service::request_context, ContextClass},
    db,
    error::{Error, Result},
    events::outbox::EventScope,
    messages::{self, MessageRow, MESSAGE_COLUMNS},
    state::AppState,
};

/// How long a held purge waits before it is re-checked.
pub const HELD_RECHECK: chrono::Duration = chrono::Duration::hours(1);

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RetentionReport {
    pub expired: usize,
    pub purged: usize,
    pub held: usize,
    pub attachments_deleted: usize,
}

/// Purges one tombstoned message inside the caller's transaction (which keeps the
/// environment's destruction lock until it ends). Returns `Ok(None)` when
/// a legal hold protects it (nothing removed), else the attachments deleted.
pub async fn purge_message<C: ConnectionTrait>(
    db: &C,
    row: &MessageRow,
    now: DateTime<Utc>,
) -> Result<Option<usize>> {
    holds::lock_for_destruction(db, row.environment_id).await?;
    if holds::message_held(db, row.id, now).await? {
        return Ok(None);
    }
    db::exec(
        db,
        "UPDATE messaging_messages SET lifecycle = 'PURGED', mls_ciphertext = NULL, managed_sealed = NULL, \
           managed_seal_nonce = NULL, managed_seal_key_id = NULL, managed_search = NULL, purged_at = $2, purge_after = NULL \
         WHERE id = $1 AND lifecycle <> 'PURGED'",
        vec![row.id.into(), now.into()],
    )
    .await?;
    // Appendix D purge_completed: ids only, never content.
    crate::events::outbox::record(
        db,
        "messaging.message.purged",
        "MESSAGE",
        row.id,
        crate::events::outbox::EventScope {
            tenant_id: row.tenant_id,
            application_id: row.application_id,
            environment_id: row.environment_id,
        },
        serde_json::json!({ "message_id": row.id, "conversation_id": row.conversation_id,
                            "security_profile": row.security_profile, "sequence": row.sequence }),
        now,
    )
    .await?;
    // Kept MANAGED revisions go with the message (the database allows this only once
    // the message is PURGED).
    db::exec(
        db,
        "DELETE FROM messaging_message_revisions WHERE message_id = $1",
        vec![row.id.into()],
    )
    .await?;
    let attachments: Vec<AttachmentRow> = db::all(
        db,
        &format!("SELECT {ATTACHMENT_COLUMNS} FROM messaging_attachments WHERE message_id = $1 AND state <> 'DELETED'"),
        vec![row.id.into()],
    )
    .await?;
    let mut n = 0;
    for a in &attachments {
        n += usize::from(delete_attachment(db, a, "MESSAGE_PURGED", now).await?);
    }
    Ok(Some(n))
}

/// One pass of the purge queue.
pub async fn purge_due(state: &AppState, batch: i64, report: &mut RetentionReport) -> Result<()> {
    let st = state.clone();
    let r = state
        .db
        .transaction::<_, RetentionReport, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let due: Vec<MessageRow> = db::all(
                    txn,
                    &format!(
                        "SELECT {MESSAGE_COLUMNS} FROM messaging_messages m WHERE m.purge_after <= $1 AND m.lifecycle NOT IN ('ACTIVE', 'PURGED') \
                         ORDER BY m.purge_after LIMIT $2 FOR UPDATE SKIP LOCKED"
                    ),
                    vec![now.into(), batch.into()],
                )
                .await?;
                let mut r = RetentionReport::default();
                for m in &due {
                    match purge_message(txn, m, now).await? {
                        Some(n) => {
                            r.purged += 1;
                            r.attachments_deleted += n;
                        }
                        None => {
                            r.held += 1;
                            db::exec(
                                txn,
                                "UPDATE messaging_messages SET purge_after = $2 WHERE id = $1",
                                vec![m.id.into(), (now + HELD_RECHECK).into()],
                            )
                            .await?;
                        }
                    }
                }
                Ok(r)
            })
        })
        .await
        .map_err(Error::from)?;
    report.purged += r.purged;
    report.held += r.held;
    report.attachments_deleted += r.attachments_deleted;
    if r.purged > 0 {
        crate::metrics::inc("messaging_messages_purged_total", &[]);
    }
    Ok(())
}

#[derive(FromQueryResult)]
struct Env {
    environment_id: Uuid,
}

/// One retention pass over every environment with a policy that limits retention.
pub async fn expire_due(state: &AppState, batch: i64, report: &mut RetentionReport) -> Result<()> {
    let envs: Vec<Env> = db::all(
        &state.db,
        "SELECT environment_id FROM messaging_application_policies ORDER BY environment_id",
        vec![],
    )
    .await?;
    for env in envs {
        let effective = match policy::current(&state.db, env.environment_id).await {
            Ok(p) => p,
            Err(_) => continue,
        };
        for &class in ContextClass::ALL.iter() {
            let days = effective
                .contexts
                .get(&class)
                .and_then(|c| c.retention_days)
                .or(effective.retention.message_days);
            let Some(days) = days else { continue };
            report.expired += expire_class(state, env.environment_id, class, days, batch).await?;
        }
    }
    report.expired += expire_conversation_overrides(state, batch).await?;
    Ok(())
}

/// §90: a conversation-specific retention (always stricter than its context's policy,
/// which the per-class pass above keeps applying) expires its own older messages.
async fn expire_conversation_overrides(state: &AppState, batch: i64) -> Result<usize> {
    let st = state.clone();
    state
        .db
        .transaction::<_, usize, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let due: Vec<MessageRow> = db::all(
                    txn,
                    &format!(
                        "SELECT {MESSAGE_COLUMNS} FROM messaging_messages m JOIN messaging_conversations c ON c.id = m.conversation_id \
                          WHERE c.retention_seconds IS NOT NULL AND m.lifecycle = 'ACTIVE' \
                            AND m.created_at < $1 - make_interval(secs => c.retention_seconds) \
                            AND m.message_kind <> 'MLS_COMMIT' \
                          ORDER BY m.created_at LIMIT $2 FOR UPDATE OF m SKIP LOCKED"
                    ),
                    vec![now.into(), batch.into()],
                )
                .await?;
                let rctx = request_context();
                for m in &due {
                    let scope = EventScope { tenant_id: m.tenant_id, application_id: m.application_id, environment_id: m.environment_id };
                    messages::tombstone(txn, &scope, m, "EXPIRED", "RETENTION_EXPIRED", now, &rctx, now).await?;
                }
                Ok(due.len())
            })
        })
        .await
        .map_err(Error::from)
}

async fn expire_class(
    state: &AppState,
    env: Uuid,
    class: ContextClass,
    days: i64,
    batch: i64,
) -> Result<usize> {
    let st = state.clone();
    state
        .db
        .transaction::<_, usize, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let cutoff = now - chrono::Duration::days(days);
                let due: Vec<MessageRow> = db::all(
                    txn,
                    &format!(
                        "SELECT {MESSAGE_COLUMNS} FROM messaging_messages m JOIN messaging_conversations c ON c.id = m.conversation_id \
                          WHERE m.environment_id = $1 AND m.lifecycle = 'ACTIVE' AND m.created_at < $2 AND c.context_class = $3 \
                            AND m.message_kind <> 'MLS_COMMIT' \
                          ORDER BY m.created_at LIMIT $4 FOR UPDATE OF m SKIP LOCKED"
                    ),
                    vec![env.into(), cutoff.into(), class.as_str().into(), batch.into()],
                )
                .await?;
                let rctx = request_context();
                for m in &due {
                    let scope = EventScope { tenant_id: m.tenant_id, application_id: m.application_id, environment_id: m.environment_id };
                    messages::tombstone(txn, &scope, m, "EXPIRED", "RETENTION_EXPIRED", now, &rctx, now).await?;
                }
                Ok(due.len())
            })
        })
        .await
        .map_err(Error::from)
}

pub async fn run_once(state: &AppState) -> Result<RetentionReport> {
    let mut r = RetentionReport::default();
    expire_due(state, 500, &mut r).await?;
    purge_due(state, 500, &mut r).await?;
    Ok(r)
}
