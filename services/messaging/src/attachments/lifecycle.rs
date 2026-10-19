//! Attachment lifecycle workers (§77): bounded passes, `FOR UPDATE SKIP LOCKED`, safe on
//! several instances, idempotent.
//!
//! * expired upload intents → EXPIRED (reservation released, partial object queued for
//!   deletion);
//! * abandoned uploads (READY, never referenced by a message within
//!   `MESSAGING_UNLINKED_ATTACHMENT_HOURS`) → DELETED;
//! * scan retries of MANAGED attachments left VERIFYING by an unavailable scanner (after
//!   the retry budget: REJECTED `SCAN_UNAVAILABLE` — never READY unscanned);
//! * the object-deletion queue: an object protected by an active legal hold is HELD, never
//!   deleted; a failing deletion retries with backoff and ends DEAD (visible, alerting) —
//!   never silently dropped;
//! * orphan reconciliation: objects under `att/` with no attachment record (a crash
//!   between upload and record, a lost deletion) are queued for deletion.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use uuid::Uuid;

use super::{AttachmentRow, ATTACHMENT_COLUMNS};
use crate::{
    db,
    error::{Error, Result},
    events::{bounded_error, RetryPolicy},
    state::AppState,
};

pub const MAX_DELETION_ATTEMPTS: i32 = 12;

/// Queues `object_key` for deletion (idempotent while a request is open).
pub async fn enqueue_deletion<C: ConnectionTrait>(
    db: &C,
    object_key: &str,
    environment_id: Uuid,
    attachment_id: Option<Uuid>,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    db::exec(
        db,
        "INSERT INTO messaging_object_deletions (id, object_key, environment_id, attachment_id, reason, next_attempt_at, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $6) ON CONFLICT DO NOTHING",
        vec![Uuid::new_v4().into(), object_key.into(), environment_id.into(), attachment_id.into(), reason.into(), now.into()],
    )
    .await?;
    Ok(())
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleReport {
    pub expired_intents: usize,
    pub abandoned: usize,
    pub rescanned: usize,
    pub deleted_objects: usize,
    pub held: usize,
    pub failed_deletions: usize,
    pub dead: usize,
    pub orphans: usize,
}

pub async fn expire_intents(state: &AppState, batch: i64) -> Result<usize> {
    let now = state.now();
    let due: Vec<AttachmentRow> = db::all(
        &state.db,
        &format!(
            "SELECT {ATTACHMENT_COLUMNS} FROM messaging_attachments WHERE state = 'INTENT_CREATED' AND intent_expires_at <= $1 \
             ORDER BY intent_expires_at LIMIT $2"
        ),
        vec![now.into(), batch.into()],
    )
    .await?;
    let mut n = 0;
    for row in due {
        let st = state.clone();
        let changed = state
            .db
            .transaction::<_, bool, Error>(move |txn| {
                Box::pin(async move {
                    let now = st.now();
                    let ok = db::exec(
                        txn,
                        "UPDATE messaging_attachments SET state = 'EXPIRED', state_reason = 'INTENT_EXPIRED', state_changed_at = $2 \
                         WHERE id = $1 AND state = 'INTENT_CREATED'",
                        vec![row.id.into(), now.into()],
                    )
                    .await?
                        == 1;
                    if ok {
                        super::quota::release(txn, &row, now).await?;
                        enqueue_deletion(txn, &row.object_key, row.environment_id, Some(row.id), "INTENT_EXPIRED", now).await?;
                    }
                    Ok(ok)
                })
            })
            .await
            .map_err(Error::from)?;
        n += usize::from(changed);
    }
    Ok(n)
}

/// Marks an attachment DELETED and queues its object (inside the caller's transaction).
pub async fn delete_attachment<C: ConnectionTrait>(
    db: &C,
    row: &AttachmentRow,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<bool> {
    let ok = db::exec(
        db,
        "UPDATE messaging_attachments SET state = 'DELETED', state_reason = $2, state_changed_at = $3 WHERE id = $1 AND state <> 'DELETED'",
        vec![row.id.into(), reason.into(), now.into()],
    )
    .await?
        == 1;
    if ok {
        super::quota::free(db, row, now).await?;
        enqueue_deletion(
            db,
            &row.object_key,
            row.environment_id,
            Some(row.id),
            reason,
            now,
        )
        .await?;
    }
    Ok(ok)
}

pub async fn expire_unlinked(state: &AppState, batch: i64) -> Result<usize> {
    let now = state.now();
    let cutoff = now - state.attachments.unlinked_ttl;
    let due: Vec<AttachmentRow> = db::all(
        &state.db,
        &format!(
            "SELECT {ATTACHMENT_COLUMNS} FROM messaging_attachments WHERE state = 'READY' AND message_id IS NULL \
               AND state_changed_at <= $1 ORDER BY state_changed_at LIMIT $2"
        ),
        vec![cutoff.into(), batch.into()],
    )
    .await?;
    let mut n = 0;
    for row in due {
        let st = state.clone();
        n += usize::from(
            state
                .db
                .transaction::<_, bool, Error>(move |txn| {
                    Box::pin(async move {
                        delete_attachment(txn, &row, "ABANDONED_UPLOAD", st.now()).await
                    })
                })
                .await
                .map_err(Error::from)?,
        );
    }
    Ok(n)
}

pub async fn rescan_due(state: &AppState, batch: i64) -> Result<usize> {
    let now = state.now();
    let due: Vec<AttachmentRow> = db::all(
        &state.db,
        &format!(
            "SELECT {ATTACHMENT_COLUMNS} FROM messaging_attachments WHERE state = 'VERIFYING' AND next_scan_at <= $1 \
             ORDER BY next_scan_at LIMIT $2"
        ),
        vec![now.into(), batch.into()],
    )
    .await?;
    let mut n = 0;
    for row in due {
        if row.scan_attempts >= state.attachments.scan_retry_max {
            let st = state.clone();
            let r = row.clone();
            state
                .db
                .transaction::<_, (), Error>(move |txn| {
                    Box::pin(async move {
                        let now = st.now();
                        if db::exec(
                            txn,
                            "UPDATE messaging_attachments SET state = 'REJECTED', state_reason = 'SCAN_UNAVAILABLE', scan_status = 'FAILED', \
                               next_scan_at = NULL, state_changed_at = $2 WHERE id = $1 AND state = 'VERIFYING'",
                            vec![r.id.into(), now.into()],
                        )
                        .await?
                            == 1
                        {
                            super::quota::release(txn, &r, now).await?;
                            enqueue_deletion(txn, &r.object_key, r.environment_id, Some(r.id), "SCAN_UNAVAILABLE", now).await?;
                        }
                        Ok(())
                    })
                })
                .await
                .map_err(Error::from)?;
            continue;
        }
        let bytes = match state
            .storage
            .get(&row.object_key, state.attachments.managed_scan_max_bytes)
            .await
        {
            Ok(Some(b)) if cf_object_storage::sha256_hex(&b) == row.declared_sha256 => b,
            _ => continue, // storage hiccup: the next pass retries
        };
        super::scan_and_settle(state, &row, "VERIFYING", &bytes).await?;
        n += 1;
    }
    Ok(n)
}

#[derive(FromQueryResult)]
struct Deletion {
    id: Uuid,
    object_key: String,
    environment_id: Uuid,
    attachment_id: Option<Uuid>,
    attempts: i32,
}

/// One pass over the deletion queue.
pub async fn process_deletions(
    state: &AppState,
    batch: i64,
    report: &mut LifecycleReport,
) -> Result<()> {
    let now = state.now();
    let token = Uuid::new_v4();
    let due: Vec<Deletion> = db::all(
        &state.db,
        "WITH due AS (SELECT id FROM messaging_object_deletions WHERE status IN ('PENDING', 'HELD') AND next_attempt_at <= $1 \
             AND (lease_expires_at IS NULL OR lease_expires_at <= $1) ORDER BY next_attempt_at LIMIT $2 FOR UPDATE SKIP LOCKED) \
         UPDATE messaging_object_deletions d SET lease_token = $3, lease_expires_at = $4 FROM due WHERE d.id = due.id \
         RETURNING d.id, d.object_key, d.environment_id, d.attachment_id, d.attempts",
        vec![now.into(), batch.into(), token.into(), (now + chrono::Duration::seconds(120)).into()],
    )
    .await?;
    let retry = RetryPolicy::default();
    for d in due {
        // Never delete an object an active legal hold protects. The check and the object
        // removal run under the environment's destruction lock (one transaction), so a
        // hold acknowledged meanwhile is never overtaken.
        let guard = state.db.begin().await?;
        crate::compliance::holds::lock_for_destruction(&guard, d.environment_id).await?;
        if let Some(a) = d.attachment_id {
            if crate::compliance::holds::attachment_held(&guard, a, now).await? {
                guard.commit().await?;
                db::exec(
                    &state.db,
                    "UPDATE messaging_object_deletions SET status = 'HELD', next_attempt_at = $3, lease_token = NULL, lease_expires_at = NULL \
                     WHERE id = $1 AND lease_token = $2",
                    vec![d.id.into(), token.into(), (now + chrono::Duration::hours(1)).into()],
                )
                .await?;
                report.held += 1;
                continue;
            }
        }
        let outcome = state.storage.delete(&d.object_key).await;
        guard.commit().await?;
        match outcome {
            Ok(()) => {
                db::exec(
                    &state.db,
                    "UPDATE messaging_object_deletions SET status = 'DONE', completed_at = $3, attempts = attempts + 1, \
                       lease_token = NULL, lease_expires_at = NULL, last_error = NULL WHERE id = $1 AND lease_token = $2",
                    vec![d.id.into(), token.into(), now.into()],
                )
                .await?;
                report.deleted_objects += 1;
                crate::metrics::inc("messaging_object_deletions_total", &[("outcome", "done")]);
            }
            Err(e) => {
                let attempts = d.attempts + 1;
                let dead = attempts >= MAX_DELETION_ATTEMPTS;
                db::exec(
                    &state.db,
                    "UPDATE messaging_object_deletions SET status = $3, attempts = $4, next_attempt_at = $5, last_error = $6, \
                       lease_token = NULL, lease_expires_at = NULL WHERE id = $1 AND lease_token = $2",
                    vec![
                        d.id.into(),
                        token.into(),
                        (if dead { "DEAD" } else { "PENDING" }).into(),
                        attempts.into(),
                        (now + retry.delay(attempts)).into(),
                        bounded_error(&e.to_string()).into(),
                    ],
                )
                .await?;
                if dead {
                    report.dead += 1;
                    crate::operations::raise(
                        &state.db,
                        crate::operations::IncidentCategory::AttachmentDeleteDeadLetter,
                        &d.id.to_string(),
                        now,
                    )
                    .await;
                    tracing::error!(
                        attempts,
                        "object deletion dead-lettered (kept for operators)"
                    );
                } else {
                    report.failed_deletions += 1;
                }
                crate::metrics::inc(
                    "messaging_object_deletions_total",
                    &[("outcome", if dead { "dead" } else { "retry" })],
                );
            }
        }
    }
    Ok(())
}

/// Lists up to `batch` objects under `att/` after `cursor` and queues orphans (no
/// attachment record) for deletion. Returns the next cursor (`None`: wrapped around).
pub async fn reconcile_orphans(
    state: &AppState,
    cursor: Option<String>,
    batch: usize,
    report: &mut LifecycleReport,
) -> Result<Option<String>> {
    let listed = match state.storage.list("att/", cursor.as_deref(), batch).await {
        Ok(l) => l,
        Err(_) => return Ok(cursor),
    };
    let now = state.now();
    let next = listed.last().map(|o| o.key.clone());
    let keys: Vec<String> = listed
        .iter()
        // Too young objects may belong to an intent being created right now.
        .filter(|o| {
            o.last_modified
                .is_none_or(|t| t < now - chrono::Duration::hours(1))
        })
        .map(|o| o.key.clone())
        .collect();
    if !keys.is_empty() {
        #[derive(FromQueryResult)]
        struct K {
            object_key: String,
        }
        let known: Vec<K> = db::all(
            &state.db,
            "SELECT object_key FROM messaging_attachments WHERE object_key = ANY($1)",
            vec![keys.clone().into()],
        )
        .await?;
        for k in keys {
            if known.iter().any(|x| x.object_key == k) {
                continue;
            }
            let env = k
                .split('/')
                .nth(1)
                .and_then(|e| Uuid::parse_str(e).ok())
                .unwrap_or(Uuid::nil());
            enqueue_deletion(&state.db, &k, env, None, "ORPHAN_OBJECT", now).await?;
            report.orphans += 1;
        }
    }
    Ok(if listed.len() < batch { None } else { next })
}

/// One full maintenance pass (tests call it directly).
pub async fn run_once(state: &AppState) -> Result<LifecycleReport> {
    let mut r = LifecycleReport {
        expired_intents: expire_intents(state, 500).await?,
        abandoned: expire_unlinked(state, 500).await?,
        rescanned: rescan_due(state, 50).await?,
        ..Default::default()
    };
    process_deletions(state, 500, &mut r).await?;
    Ok(r)
}

pub async fn run(state: AppState, interval: std::time::Duration) {
    let mut cursor: Option<String> = None;
    let mut ticks = 0u64;
    loop {
        match run_once(&state).await {
            Ok(r) => {
                if r != LifecycleReport::default() {
                    tracing::info!(
                        expired = r.expired_intents,
                        abandoned = r.abandoned,
                        deleted = r.deleted_objects,
                        held = r.held,
                        dead = r.dead,
                        "attachment lifecycle pass"
                    );
                }
            }
            Err(e) => tracing::warn!(error = %e, "attachment lifecycle pass failed"),
        }
        ticks += 1;
        if ticks.is_multiple_of(10) {
            let mut r = LifecycleReport::default();
            match reconcile_orphans(&state, cursor.clone(), 1000, &mut r).await {
                Ok(c) => cursor = c,
                Err(e) => tracing::warn!(error = %e, "orphan reconciliation failed"),
            }
        }
        tokio::time::sleep(interval).await;
    }
}
