//! Server-side receipt-event coalescing (audit MSG-018, server half).
//!
//! Read and delivery receipts are shared with the other members of a conversation as the
//! durable conversation events `messaging.read-cursor.updated` and
//! `messaging.delivery-cursor.updated`. Publishing one per read / acknowledgement took the
//! conversation's event-sequence row lock (the same lock as every send) and fanned out to
//! every member, for cursors that a few hundred milliseconds later were superseded.
//!
//! Receipts are now coalesced per conversation, member and receipt kind
//! (`messaging_receipt_emissions`), as a throttle with a leading and a trailing edge:
//!
//! * the first advance of a window is published at once, in the caller's transaction
//!   (sparse reads are as fast as before);
//! * further advances inside the window (`RealtimeSettings::receipt_window`) only raise
//!   the row's `pending_sequence` — no conversation lock, no event;
//! * when the window ends, [`flush_due`] publishes ONE event with the highest cursor.
//!
//! Correctness does not depend on any intermediate event: the canonical cursors stay in
//! `messaging_read_cursors` / `messaging_received_cursors` (monotonic, enforced by the
//! database), the published cursor only grows (enforced by the database), a pending
//! cursor is durable (a crash publishes it on the next pass of any instance), and a
//! reconnecting client reads the current cursors with the `receipts.query` realtime frame
//! ([`snapshot`]) instead of relying on the receipts it missed. Privacy is re-checked
//! when a coalesced receipt is published (a member who stopped sharing read receipts
//! inside the window publishes nothing more), and only ACTIVE members' receipts are
//! published.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    conversations::{policy, service::request_context},
    db,
    error::Result,
    events::outbox::{self, EventScope, NewEvent},
    state::AppState,
};

/// What a receipt reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptKind {
    /// `messaging.read-cursor.updated` (`read_sequence`).
    Read,
    /// `messaging.delivery-cursor.updated` (`delivered_sequence`).
    Delivered,
}

impl ReceiptKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ReceiptKind::Read => "READ",
            ReceiptKind::Delivered => "DELIVERED",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "READ" => Some(ReceiptKind::Read),
            "DELIVERED" => Some(ReceiptKind::Delivered),
            _ => None,
        }
    }

    fn event_type(self) -> &'static str {
        match self {
            ReceiptKind::Read => "messaging.read-cursor.updated",
            ReceiptKind::Delivered => "messaging.delivery-cursor.updated",
        }
    }

    fn payload(self, principal_id: Uuid, sequence: i64) -> Value {
        match self {
            ReceiptKind::Read => json!({ "principal_id": principal_id, "read_sequence": sequence }),
            ReceiptKind::Delivered => {
                json!({ "principal_id": principal_id, "delivered_sequence": sequence })
            }
        }
    }
}

#[derive(Debug, FromQueryResult)]
struct EmissionRow {
    emitted_sequence: i64,
    open: bool,
}

async fn publish<C: ConnectionTrait>(
    txn: &C,
    kind: ReceiptKind,
    scope: EventScope,
    conversation_id: Uuid,
    principal_id: Uuid,
    sequence: i64,
    now: DateTime<Utc>,
) -> Result<()> {
    outbox::enqueue_conversation(
        txn,
        conversation_id,
        NewEvent {
            event_type: kind.event_type(),
            aggregate_type: "CONVERSATION",
            aggregate_id: conversation_id,
            scope,
            payload: kind.payload(principal_id, sequence),
        },
        &request_context(),
        now,
    )
    .await?;
    crate::metrics::inc(
        "messaging_receipt_events_total",
        &[("kind", kind.as_str()), ("edge", "leading")],
    );
    Ok(())
}

/// Records that `principal_id`'s `kind` cursor in `conversation_id` is now `sequence`
/// (already committed to the canonical cursor table in the same transaction, and already
/// cleared for sharing by the caller). Publishes the receipt at once when no receipt of
/// this member and kind was published inside the window; otherwise defers it to the end
/// of the window, keeping only the highest cursor. Returns whether an event was written.
#[allow(clippy::too_many_arguments)]
pub async fn record<C: ConnectionTrait>(
    txn: &C,
    window_secs: f64,
    kind: ReceiptKind,
    scope: EventScope,
    conversation_id: Uuid,
    principal_id: Uuid,
    sequence: i64,
    now: DateTime<Utc>,
) -> Result<bool> {
    if sequence <= 0 {
        // Nothing read / delivered yet: there is no position to share.
        return Ok(false);
    }
    db::exec(
        txn,
        "INSERT INTO messaging_receipt_emissions (conversation_id, principal_id, receipt_kind, tenant_id, \
           application_id, environment_id, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT DO NOTHING",
        vec![
            conversation_id.into(),
            principal_id.into(),
            kind.as_str().into(),
            scope.tenant_id.into(),
            scope.application_id.into(),
            scope.environment_id.into(),
            now.into(),
        ],
    )
    .await?;
    // The row lock serializes this member's receipts of this kind (not the conversation).
    let row: EmissionRow = db::one(
        txn,
        "SELECT emitted_sequence, (due_at IS NULL AND emitted_at <= $4::timestamptz - make_interval(secs => $5)) AS open \
           FROM messaging_receipt_emissions WHERE conversation_id = $1 AND principal_id = $2 AND receipt_kind = $3 FOR UPDATE",
        vec![
            conversation_id.into(),
            principal_id.into(),
            kind.as_str().into(),
            now.into(),
            window_secs.into(),
        ],
    )
    .await?
    .ok_or(crate::error::Error::code(crate::error::ErrorCode::InternalError))?;
    if sequence <= row.emitted_sequence {
        // Already published (a repeated or older cursor): nothing new to tell anyone.
        return Ok(false);
    }
    if row.open {
        publish(
            txn,
            kind,
            scope,
            conversation_id,
            principal_id,
            sequence,
            now,
        )
        .await?;
        db::exec(
            txn,
            "UPDATE messaging_receipt_emissions SET emitted_sequence = $4, emitted_at = $5, updated_at = $5 \
             WHERE conversation_id = $1 AND principal_id = $2 AND receipt_kind = $3",
            vec![
                conversation_id.into(),
                principal_id.into(),
                kind.as_str().into(),
                sequence.into(),
                now.into(),
            ],
        )
        .await?;
        return Ok(true);
    }
    db::exec(
        txn,
        "UPDATE messaging_receipt_emissions SET pending_sequence = GREATEST(COALESCE(pending_sequence, 0), $4), \
           due_at = COALESCE(due_at, GREATEST(emitted_at + make_interval(secs => $6), $5::timestamptz)), updated_at = $5 \
         WHERE conversation_id = $1 AND principal_id = $2 AND receipt_kind = $3",
        vec![
            conversation_id.into(),
            principal_id.into(),
            kind.as_str().into(),
            sequence.into(),
            now.into(),
            window_secs.into(),
        ],
    )
    .await?;
    crate::metrics::inc(
        "messaging_receipt_events_coalesced_total",
        &[("kind", kind.as_str())],
    );
    Ok(false)
}

#[derive(Debug, FromQueryResult)]
struct DueRow {
    conversation_id: Uuid,
    principal_id: Uuid,
    receipt_kind: String,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    pending_sequence: i64,
    shares_read: bool,
    active_member: bool,
}

/// Publishes every coalesced receipt whose window has ended (bounded batch; multi-instance
/// safe through `FOR UPDATE SKIP LOCKED`; rows are taken in conversation order so two
/// instances never wait on each other's conversation locks in opposite orders). Returns
/// the number of events published.
pub async fn flush_due(state: &AppState, batch: i64) -> Result<usize> {
    let now = state.now();
    let txn = state.db.begin().await?;
    let due: Vec<DueRow> = db::all(
        &txn,
        "SELECT r.conversation_id, r.principal_id, r.receipt_kind, r.tenant_id, r.application_id, r.environment_id, \
                r.pending_sequence, \
                (c.read_receipts IS DISTINCT FROM FALSE AND NOT EXISTS (SELECT 1 FROM messaging_presence_settings s \
                   WHERE s.environment_id = r.environment_id AND s.principal_id = r.principal_id AND NOT s.share_read_receipts)) \
                  AS shares_read, \
                EXISTS (SELECT 1 FROM messaging_conversation_members m WHERE m.conversation_id = r.conversation_id \
                   AND m.principal_id = r.principal_id AND m.state = 'ACTIVE') AS active_member \
           FROM messaging_receipt_emissions r JOIN messaging_conversations c ON c.id = r.conversation_id \
          WHERE r.due_at <= $1 \
          ORDER BY r.conversation_id, r.principal_id, r.receipt_kind LIMIT $2 FOR UPDATE OF r SKIP LOCKED",
        vec![now.into(), batch.into()],
    )
    .await?;
    let mut policies: HashMap<Uuid, policy::EffectivePolicy> = HashMap::new();
    let mut published = 0;
    for row in &due {
        let Some(kind) = ReceiptKind::parse(&row.receipt_kind) else {
            continue;
        };
        let privacy = match policies.entry(row.environment_id) {
            std::collections::hash_map::Entry::Occupied(e) => &e.into_mut().privacy,
            std::collections::hash_map::Entry::Vacant(e) => {
                &e.insert(policy::current(&txn, row.environment_id).await?)
                    .privacy
            }
        };
        let shared = row.active_member
            && match kind {
                ReceiptKind::Read => privacy.read_receipts && row.shares_read,
                ReceiptKind::Delivered => privacy.delivery_receipts,
            };
        if shared {
            outbox::enqueue_conversation(
                &txn,
                row.conversation_id,
                NewEvent {
                    event_type: kind.event_type(),
                    aggregate_type: "CONVERSATION",
                    aggregate_id: row.conversation_id,
                    scope: EventScope {
                        tenant_id: row.tenant_id,
                        application_id: row.application_id,
                        environment_id: row.environment_id,
                    },
                    payload: kind.payload(row.principal_id, row.pending_sequence),
                },
                &request_context(),
                now,
            )
            .await?;
            published += 1;
            crate::metrics::inc(
                "messaging_receipt_events_total",
                &[("kind", kind.as_str()), ("edge", "trailing")],
            );
        } else {
            crate::metrics::inc(
                "messaging_receipt_events_withheld_total",
                &[("kind", kind.as_str())],
            );
        }
        // Withheld or published, the window is closed; the canonical cursor is unchanged.
        db::exec(
            &txn,
            "UPDATE messaging_receipt_emissions SET emitted_sequence = CASE WHEN $4 THEN pending_sequence ELSE emitted_sequence END, \
               emitted_at = CASE WHEN $4 THEN $5 ELSE emitted_at END, pending_sequence = NULL, due_at = NULL, updated_at = $5 \
             WHERE conversation_id = $1 AND principal_id = $2 AND receipt_kind = $3",
            vec![
                row.conversation_id.into(),
                row.principal_id.into(),
                kind.as_str().into(),
                shared.into(),
                now.into(),
            ],
        )
        .await?;
    }
    txn.commit().await?;
    if published > 0 {
        state.realtime.outbox_wakeup.notify_one();
    }
    Ok(published)
}

/// Runs [`flush_due`] every `interval` until the process stops.
pub async fn run(state: AppState, interval: std::time::Duration) {
    loop {
        match flush_due(&state, 500).await {
            Ok(n) if n >= 500 => continue,
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "receipt coalescing pass failed"),
        }
        tokio::time::sleep(interval).await;
    }
}

#[derive(Debug, FromQueryResult)]
struct SnapshotRow {
    principal_id: Uuid,
    read_sequence: Option<i64>,
    delivered_sequence: Option<i64>,
    shares_read: bool,
}

/// The most members a snapshot lists (ordered by principal id; `truncated` beyond it).
pub const SNAPSHOT_LIMIT: i64 = 1000;

/// The CURRENT receipt cursors of the other ACTIVE members of a conversation, filtered
/// exactly like the published events (environment policy, the conversation's
/// `readReceipts`, each member's `shareReadReceipts`). What a reconnecting client reads
/// instead of the receipts it missed (coalesced or not).
pub async fn snapshot<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    conversation_id: Uuid,
    viewer: Uuid,
) -> Result<Value> {
    let effective = policy::current(db, environment_id).await?;
    let rows: Vec<SnapshotRow> = db::all(
        db,
        "SELECT m.principal_id, r.read_sequence, d.delivered_sequence, \
                (c.read_receipts IS DISTINCT FROM FALSE AND NOT EXISTS (SELECT 1 FROM messaging_presence_settings s \
                   WHERE s.environment_id = c.environment_id AND s.principal_id = m.principal_id AND NOT s.share_read_receipts)) \
                  AS shares_read \
           FROM messaging_conversation_members m JOIN messaging_conversations c ON c.id = m.conversation_id \
           LEFT JOIN messaging_read_cursors r ON r.conversation_id = m.conversation_id AND r.principal_id = m.principal_id \
           LEFT JOIN messaging_received_cursors d ON d.conversation_id = m.conversation_id AND d.principal_id = m.principal_id \
          WHERE m.conversation_id = $1 AND m.state = 'ACTIVE' AND m.principal_id <> $2 \
            AND (r.read_sequence IS NOT NULL OR d.delivered_sequence IS NOT NULL) \
          ORDER BY m.principal_id LIMIT $3",
        vec![
            conversation_id.into(),
            viewer.into(),
            (SNAPSHOT_LIMIT + 1).into(),
        ],
    )
    .await?;
    let truncated = rows.len() as i64 > SNAPSHOT_LIMIT;
    let mut members = Vec::new();
    for r in rows.into_iter().take(SNAPSHOT_LIMIT as usize) {
        let read = r
            .read_sequence
            .filter(|_| effective.privacy.read_receipts && r.shares_read);
        let delivered = r
            .delivered_sequence
            .filter(|_| effective.privacy.delivery_receipts);
        if read.is_none() && delivered.is_none() {
            continue;
        }
        let mut m = json!({ "principalId": r.principal_id });
        if let Some(v) = read {
            m["readSequence"] = json!(v);
        }
        if let Some(v) = delivered {
            m["deliveredSequence"] = json!(v);
        }
        members.push(m);
    }
    Ok(json!({ "members": members, "truncated": truncated }))
}
