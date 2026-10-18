//! Data deletion requests (§94), executed by a leased worker.
//!
//! * `PRINCIPAL_ERASURE` — every message the principal sent (tombstoned `DELETED` /
//!   `DATA_ERASURE` with its durable event, then purged), their unsent uploads, their
//!   presence preference and shared last-seen time.
//! * `CONVERSATION_PURGE` — every message and attachment of one conversation.
//!
//! Legal holds win: a held message is still tombstoned (no member sees it again) but its
//! stored ciphertext / text and attachments are preserved; held items are counted and
//! the request ends `PARTIALLY_HELD` (re-request after the hold is released).
//! MLS handshake records (commits) are retained in both kinds: they are protocol state
//! other members' devices may still need to reach the current epoch and carry no
//! application content; the response says so (`handshakeRecordsRetained`).

use chrono::{DateTime, Utc};
use sea_orm::{FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{holds, retention};
use crate::{
    attachments::{lifecycle::delete_attachment, AttachmentRow, ATTACHMENT_COLUMNS},
    audit::{self, AuditEntry},
    conversations::service::{self, request_context, rfc3339},
    db,
    error::{Error, ErrorCode, Result},
    events::outbox::EventScope,
    identity::TrustedContext,
    messages::{self, MessageRow, MESSAGE_COLUMNS},
    state::AppState,
};

pub const CHUNK: usize = 200;

#[derive(Debug, Clone, FromQueryResult)]
pub struct DeletionRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub kind: String,
    pub principal_id: Option<Uuid>,
    pub conversation_id: Option<Uuid>,
    pub requested_by_credential_id: Uuid,
    pub status: String,
    pub messages_purged: i64,
    pub attachments_deleted: i64,
    pub held_items: i64,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

const COLUMNS: &str = "id, tenant_id, application_id, environment_id, kind, principal_id, conversation_id, requested_by_credential_id, \
     status, messages_purged, \
     attachments_deleted, held_items, created_at, completed_at";

fn view(d: &DeletionRow) -> Value {
    json!({
        "requestId": d.id,
        "kind": d.kind,
        "principalId": d.principal_id,
        "conversationId": d.conversation_id,
        "status": d.status,
        "messagesPurged": d.messages_purged,
        "attachmentsDeleted": d.attachments_deleted,
        "heldItems": d.held_items,
        "handshakeRecordsRetained": true,
        "createdAt": rfc3339(d.created_at),
        "completedAt": d.completed_at.map(rfc3339),
    })
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeletionRequest {
    pub kind: String,
    #[serde(default)]
    pub principal_id: Option<Uuid>,
    #[serde(default)]
    pub conversation_id: Option<Uuid>,
}

/// `POST /v1/messaging/deletion-requests`.
pub async fn create(
    state: &AppState,
    ctx: &TrustedContext,
    request: DeletionRequest,
) -> Result<Value> {
    let now = state.now();
    match (
        request.kind.as_str(),
        request.principal_id,
        request.conversation_id,
    ) {
        ("PRINCIPAL_ERASURE", Some(_), None) => {}
        ("CONVERSATION_PURGE", None, Some(c)) => {
            service::load(&state.db, ctx, c, false)
                .await?
                .ok_or(Error::code(ErrorCode::NotFound))?;
        }
        _ => {
            return Err(Error::validation(
                "kind PRINCIPAL_ERASURE needs principalId; CONVERSATION_PURGE needs conversationId",
            ))
        }
    }
    let id = Uuid::new_v4();
    db::exec(
        &state.db,
        "INSERT INTO messaging_deletion_requests (id, tenant_id, application_id, environment_id, kind, principal_id, conversation_id, \
           requested_by_credential_id, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        vec![
            id.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            request.kind.clone().into(),
            request.principal_id.into(),
            request.conversation_id.into(),
            ctx.credential_id.into(),
            now.into(),
        ],
    )
    .await?;
    audit::record(
        &state.db,
        AuditEntry {
            ctx,
            principal_id: None,
            action: "DATA_DELETION_REQUESTED",
            target_kind: "DELETION_REQUEST",
            target_id: id,
            detail: json!({ "kind": request.kind, "principal_id": request.principal_id, "conversation_id": request.conversation_id }),
        },
        &request_context(),
        now,
    )
    .await?;
    get(state, ctx, id).await
}

pub async fn get(state: &AppState, ctx: &TrustedContext, id: Uuid) -> Result<Value> {
    let d: DeletionRow = db::one(
        &state.db,
        &format!(
            "SELECT {COLUMNS} FROM messaging_deletion_requests WHERE id = $1 AND tenant_id = $2 AND application_id = $3 AND environment_id = $4"
        ),
        vec![id.into(), ctx.scope.tenant_id.into(), ctx.scope.application_id.into(), ctx.scope.environment_id.into()],
    )
    .await?
    .ok_or(Error::code(ErrorCode::NotFound))?;
    Ok(view(&d))
}

#[derive(Default)]
struct Counts {
    purged: i64,
    attachments: i64,
    held: i64,
}

async fn process(state: &AppState, d: &DeletionRow) -> Result<Counts> {
    let mut counts = Counts::default();
    #[derive(FromQueryResult)]
    struct Id {
        id: Uuid,
    }
    let (filter, arg) = match (d.principal_id, d.conversation_id) {
        (Some(p), _) => ("m.environment_id = $1 AND m.sender_principal_id = $2", p),
        (_, Some(c)) => ("m.environment_id = $1 AND m.conversation_id = $2", c),
        _ => return Ok(counts),
    };
    let ids: Vec<Id> = db::all(
        &state.db,
        &format!(
            "SELECT m.id FROM messaging_messages m WHERE {filter} AND m.lifecycle <> 'PURGED' AND m.message_kind <> 'MLS_COMMIT' \
             ORDER BY m.created_at, m.id"
        ),
        vec![d.environment_id.into(), arg.into()],
    )
    .await?;
    for chunk in ids.chunks(CHUNK) {
        let ids: Vec<Uuid> = chunk.iter().map(|i| i.id).collect();
        let st = state.clone();
        let c = state
            .db
            .transaction::<_, (i64, i64, i64), Error>(move |txn| {
                Box::pin(async move {
                    let now = st.now();
                    let rctx = request_context();
                    let rows: Vec<MessageRow> = db::all(
                        txn,
                        &format!("SELECT {MESSAGE_COLUMNS} FROM messaging_messages m WHERE m.id = ANY($1) ORDER BY m.sequence FOR UPDATE"),
                        vec![ids.into()],
                    )
                    .await?;
                    let (mut purged, mut atts, mut held) = (0, 0, 0);
                    for m in rows {
                        if m.is_active() {
                            let scope = EventScope { tenant_id: m.tenant_id, application_id: m.application_id, environment_id: m.environment_id };
                            messages::tombstone(txn, &scope, &m, "DELETED", "DATA_ERASURE", now, &rctx, now).await?;
                        }
                        match retention::purge_message(txn, &m, now).await? {
                            Some(n) => {
                                purged += 1;
                                atts += n as i64;
                            }
                            None => held += 1,
                        }
                    }
                    Ok((purged, atts, held))
                })
            })
            .await
            .map_err(Error::from)?;
        counts.purged += c.0;
        counts.attachments += c.1;
        counts.held += c.2;
    }
    // Uploads never sent in a message.
    let column = if d.principal_id.is_some() {
        "uploader_principal_id"
    } else {
        "conversation_id"
    };
    let unsent: Vec<AttachmentRow> = db::all(
        &state.db,
        &format!(
            "SELECT {ATTACHMENT_COLUMNS} FROM messaging_attachments WHERE environment_id = $1 AND {column} = $2 \
               AND message_id IS NULL AND state NOT IN ('DELETED', 'EXPIRED', 'REJECTED')"
        ),
        vec![d.environment_id.into(), arg.into()],
    )
    .await?;
    for a in unsent {
        let now = state.now();
        if holds::attachment_held(&state.db, a.id, now).await? {
            counts.held += 1;
            continue;
        }
        let st = state.clone();
        let deleted = state
            .db
            .transaction::<_, bool, Error>(move |txn| {
                Box::pin(async move { delete_attachment(txn, &a, "DATA_ERASURE", st.now()).await })
            })
            .await
            .map_err(Error::from)?;
        counts.attachments += i64::from(deleted);
    }
    if let Some(p) = d.principal_id {
        for sql in [
            "DELETE FROM messaging_presence_settings WHERE environment_id = $1 AND principal_id = $2",
            "DELETE FROM messaging_presence_last_seen WHERE environment_id = $1 AND principal_id = $2",
        ] {
            db::exec(&state.db, sql, vec![d.environment_id.into(), p.into()]).await?;
        }
    }
    Ok(counts)
}

/// Runs at most `batch` pending requests (leased).
pub async fn run_pending(state: &AppState, batch: i64) -> Result<usize> {
    let now = state.now();
    let token = Uuid::new_v4();
    let due: Vec<DeletionRow> = db::all(
        &state.db,
        &format!(
            "WITH due AS (SELECT id FROM messaging_deletion_requests WHERE status = 'PENDING' \
                 AND (lease_expires_at IS NULL OR lease_expires_at <= $1) ORDER BY created_at LIMIT $2 FOR UPDATE SKIP LOCKED) \
             UPDATE messaging_deletion_requests r SET lease_token = $3, lease_expires_at = $4 FROM due WHERE r.id = due.id RETURNING {}",
            COLUMNS.split(", ").map(|c| format!("r.{c}")).collect::<Vec<_>>().join(", ")
        ),
        vec![now.into(), batch.into(), token.into(), (now + chrono::Duration::minutes(30)).into()],
    )
    .await?;
    let n = due.len();
    for d in due {
        let c = process(state, &d).await?;
        let now = state.now();
        let status = if c.held > 0 {
            "PARTIALLY_HELD"
        } else {
            "COMPLETED"
        };
        db::exec(
            &state.db,
            "UPDATE messaging_deletion_requests SET status = $3, messages_purged = $4, attachments_deleted = $5, held_items = $6, \
               completed_at = $7, lease_token = NULL, lease_expires_at = NULL WHERE id = $1 AND lease_token = $2",
            vec![d.id.into(), token.into(), status.into(), c.purged.into(), c.attachments.into(), c.held.into(), now.into()],
        )
        .await?;
        audit::record_for_credential(
            &state.db,
            (d.tenant_id, d.application_id, d.environment_id),
            d.requested_by_credential_id,
            "DATA_DELETION_COMPLETED",
            "DELETION_REQUEST",
            d.id,
            json!({ "outcome": status, "messages_purged": c.purged, "attachments_deleted": c.attachments, "held_items": c.held }),
            &request_context(),
            now,
        )
        .await?;
    }
    Ok(n)
}
