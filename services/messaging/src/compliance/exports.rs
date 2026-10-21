//! Authorized exports (§93). MANAGED conversations only: the server exports what it can
//! legitimately read. A PRIVATE_E2EE conversation is refused with
//! `E2EE_EXPORT_NOT_AVAILABLE` — there is no server-side path to its plaintext; a member
//! exports from an authorized device.
//!
//! The export is a leased background job that writes ONE JSON object to protected
//! storage (`exp/<environment>/<export>`), downloadable through short-lived signed
//! grants (forced download) until it expires, then deleted through the deletion queue.
//! Requests, grants and expiry are audited.

use chrono::{DateTime, Utc};
use sea_orm::FromQueryResult;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    attachments::{lifecycle::enqueue_deletion, AttachmentRow, ATTACHMENT_COLUMNS},
    audit::{self, AuditEntry},
    conversations::{
        policy,
        service::{self, request_context, rfc3339},
        ContextClass,
    },
    db,
    domain::SecurityProfile,
    error::{Error, ErrorCode, Result},
    identity::TrustedContext,
    messages::{managed, MessageRow, MESSAGE_COLUMNS},
    state::AppState,
};

pub const FORMAT: &str = "CFMEET_MESSAGING_MANAGED_EXPORT_V1";
pub const DOWNLOAD_DAYS: i64 = 7;
pub const MAX_MESSAGES: i64 = 100_000;
pub const MAX_ATTEMPTS: i32 = 5;

#[derive(Debug, Clone, FromQueryResult)]
pub struct ExportRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub conversation_id: Uuid,
    pub status: String,
    pub object_key: String,
    pub size: Option<i64>,
    pub sha256: Option<String>,
    pub message_count: Option<i64>,
    pub failure: Option<String>,
    pub attempts: i32,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
}

const COLUMNS: &str = "id, tenant_id, application_id, environment_id, conversation_id, status, object_key, size, sha256, \
     message_count, failure, attempts, created_at, completed_at, expires_at";

fn view(e: &ExportRow) -> Value {
    json!({
        "exportId": e.id,
        "conversationId": e.conversation_id,
        "format": FORMAT,
        "status": e.status,
        "size": e.size,
        "sha256": e.sha256,
        "messageCount": e.message_count,
        "failure": e.failure,
        "createdAt": rfc3339(e.created_at),
        "completedAt": e.completed_at.map(rfc3339),
        "expiresAt": rfc3339(e.expires_at),
    })
}

async fn load(state: &AppState, ctx: &TrustedContext, id: Uuid) -> Result<ExportRow> {
    db::one(
        &state.db,
        &format!("SELECT {COLUMNS} FROM messaging_exports WHERE id = $1 AND tenant_id = $2 AND application_id = $3 AND environment_id = $4"),
        vec![id.into(), ctx.scope.tenant_id.into(), ctx.scope.application_id.into(), ctx.scope.environment_id.into()],
    )
    .await?
    .ok_or(Error::code(ErrorCode::NotFound))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportRequest {
    pub conversation_id: Uuid,
}

/// `POST /v1/messaging/exports`.
pub async fn request(
    state: &AppState,
    ctx: &TrustedContext,
    request: ExportRequest,
) -> Result<Value> {
    crate::operations::require_open(
        &state.db,
        crate::operations::KillSwitch::Exports,
        ctx.scope.environment_id,
    )
    .await?;
    let now = state.now();
    let conversation = service::load(&state.db, ctx, request.conversation_id, false)
        .await?
        .ok_or(Error::code(ErrorCode::NotFound))?;
    if conversation.profile() == SecurityProfile::PrivateE2ee {
        audit::record(
            &state.db,
            AuditEntry {
                ctx,
                principal_id: None,
                action: "EXPORT_REFUSED",
                target_kind: "CONVERSATION",
                target_id: conversation.id,
                detail: json!({ "outcome": "E2EE_EXPORT_NOT_AVAILABLE" }),
            },
            &request_context(),
            now,
        )
        .await?;
        return Err(Error::code(ErrorCode::E2eeExportNotAvailable));
    }
    let effective = policy::current(&state.db, ctx.scope.environment_id).await?;
    let enabled = ContextClass::parse(&conversation.context_class)
        .and_then(|c| effective.contexts.get(&c))
        .is_some_and(|c| c.exports_enabled);
    if !enabled {
        return Err(Error::reason(
            ErrorCode::FeatureDisabled,
            "EXPORTS_DISABLED",
        ));
    }
    if !state.storage.configured() {
        return Err(Error::reason(
            ErrorCode::FeatureDisabled,
            "EXPORT_STORAGE_NOT_CONFIGURED",
        ));
    }
    if state.managed_keys.is_none() {
        return Err(Error::reason(
            ErrorCode::FeatureDisabled,
            "MANAGED_KEYS_NOT_CONFIGURED",
        ));
    }
    let id = Uuid::new_v4();
    let key = format!("exp/{}/{}", ctx.scope.environment_id.simple(), id.simple());
    db::exec(
        &state.db,
        "INSERT INTO messaging_exports (id, tenant_id, application_id, environment_id, conversation_id, requested_by_credential_id, \
           object_key, created_at, expires_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        vec![
            id.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            conversation.id.into(),
            ctx.credential_id.into(),
            key.into(),
            now.into(),
            (now + chrono::Duration::days(DOWNLOAD_DAYS)).into(),
        ],
    )
    .await?;
    audit::record(
        &state.db,
        AuditEntry {
            ctx,
            principal_id: None,
            action: "EXPORT_REQUESTED",
            target_kind: "EXPORT",
            target_id: id,
            detail: json!({ "conversation_id": conversation.id }),
        },
        &request_context(),
        now,
    )
    .await?;
    Ok(view(&load(state, ctx, id).await?))
}

pub async fn get(state: &AppState, ctx: &TrustedContext, id: Uuid) -> Result<Value> {
    Ok(view(&load(state, ctx, id).await?))
}

/// `POST /v1/messaging/exports/{id}/download-grants`.
pub async fn grant(state: &AppState, ctx: &TrustedContext, id: Uuid) -> Result<Value> {
    crate::operations::require_open(
        &state.db,
        crate::operations::KillSwitch::Exports,
        ctx.scope.environment_id,
    )
    .await?;
    let now = state.now();
    let e = load(state, ctx, id).await?;
    if e.status != "READY" || e.expires_at <= now {
        return Err(Error::reason(ErrorCode::Conflict, "EXPORT_NOT_READY"));
    }
    let signed = state
        .storage
        .signed_read_as(
            &e.object_key,
            state.attachments.grant_ttl,
            now,
            &cf_object_storage::ResponseOverrides {
                content_type: Some("application/json".into()),
                content_disposition: Some(format!(
                    "attachment; filename=\"messaging-export-{}.json\"",
                    e.id
                )),
            },
        )
        .map_err(|_| Error::code(ErrorCode::StorageUnavailable))?;
    audit::record(
        &state.db,
        AuditEntry {
            ctx,
            principal_id: None,
            action: "EXPORT_DOWNLOAD_GRANTED",
            target_kind: "EXPORT",
            target_id: e.id,
            detail: json!({ "conversation_id": e.conversation_id }),
        },
        &request_context(),
        now,
    )
    .await?;
    Ok(
        json!({ "exportId": e.id, "method": "GET", "url": signed.url, "expiresAt": rfc3339(signed.expires_at), "size": e.size, "sha256": e.sha256 }),
    )
}

async fn build(
    state: &AppState,
    e: &ExportRow,
    now: DateTime<Utc>,
) -> std::result::Result<(Vec<u8>, i64), &'static str> {
    let count = db::scalar_i64(
        &state.db,
        "SELECT count(*) FROM messaging_messages WHERE conversation_id = $1",
        vec![e.conversation_id.into()],
    )
    .await
    .map_err(|_| "DATABASE_UNAVAILABLE")?;
    if count > MAX_MESSAGES {
        return Err("TOO_LARGE");
    }
    let rows: Vec<MessageRow> = db::all(
        &state.db,
        &format!("SELECT {MESSAGE_COLUMNS} FROM messaging_messages m WHERE m.conversation_id = $1 ORDER BY m.sequence"),
        vec![e.conversation_id.into()],
    )
    .await
    .map_err(|_| "DATABASE_UNAVAILABLE")?;
    if rows.iter().any(|r| r.profile() != SecurityProfile::Managed) {
        return Err("NOT_MANAGED");
    }
    let attachments: Vec<AttachmentRow> = db::all(
        &state.db,
        &format!("SELECT {ATTACHMENT_COLUMNS} FROM messaging_attachments WHERE conversation_id = $1 AND message_id IS NOT NULL"),
        vec![e.conversation_id.into()],
    )
    .await
    .map_err(|_| "DATABASE_UNAVAILABLE")?;
    #[derive(FromQueryResult)]
    struct M {
        principal_id: Uuid,
        role: String,
        state: String,
    }
    let members: Vec<M> = db::all(
        &state.db,
        "SELECT principal_id, role, state FROM messaging_conversation_members WHERE conversation_id = $1 ORDER BY joined_at, principal_id",
        vec![e.conversation_id.into()],
    )
    .await
    .map_err(|_| "DATABASE_UNAVAILABLE")?;
    let mut messages = Vec::with_capacity(rows.len());
    for r in &rows {
        let text = if r.is_active() {
            Some(managed::text_of(state, r).ok_or("SEAL_UNREADABLE")?)
        } else {
            None
        };
        // §46: prior versions of an edited message (kept sealed; opened for the export).
        let mut revisions = Vec::new();
        if r.is_active() && r.edit_count > 0 {
            for v in managed::revisions_of(state, &state.db, r)
                .await
                .map_err(|_| "DATABASE_UNAVAILABLE")?
            {
                revisions.push(json!({
                    "revision": v.revision,
                    "text": v.text.ok_or("SEAL_UNREADABLE")?,
                    "createdAt": rfc3339(v.created_at),
                    "supersededAt": rfc3339(v.superseded_at),
                }));
            }
        }
        let atts: Vec<Value> = attachments
            .iter()
            .filter(|a| a.message_id == Some(r.id))
            .map(|a| json!({ "attachmentId": a.id, "kind": a.kind, "mediaType": a.media_type, "fileName": a.safe_file_name,
                             "size": a.declared_size, "sha256": a.declared_sha256, "state": a.state }))
            .collect();
        messages.push(json!({
            "messageId": r.id, "sequence": r.sequence, "kind": r.message_kind, "senderPrincipalId": r.sender_principal_id,
            "createdAt": rfc3339(r.created_at), "lifecycle": r.lifecycle, "editCount": r.edit_count,
            "editedAt": r.edited_at.map(rfc3339), "text": text, "revisions": revisions, "attachments": atts,
        }));
    }
    let doc = json!({
        "format": FORMAT,
        "exportId": e.id,
        "conversationId": e.conversation_id,
        "securityProfile": "MANAGED",
        "exportedAt": rfc3339(now),
        "members": members.iter().map(|m| json!({ "principalId": m.principal_id, "role": m.role, "state": m.state })).collect::<Vec<_>>(),
        "messages": messages,
    });
    Ok((
        serde_json::to_vec_pretty(&doc).map_err(|_| "ENCODING")?,
        rows.len() as i64,
    ))
}

/// Runs at most `batch` pending exports (leased; safe on several instances).
pub async fn run_pending(state: &AppState, batch: i64) -> Result<usize> {
    let now = state.now();
    let token = Uuid::new_v4();
    let due: Vec<ExportRow> = db::all(
        &state.db,
        &format!(
            "WITH due AS (SELECT id FROM messaging_exports WHERE status = 'PENDING' AND (lease_expires_at IS NULL OR lease_expires_at <= $1) \
                 ORDER BY created_at LIMIT $2 FOR UPDATE SKIP LOCKED) \
             UPDATE messaging_exports e SET lease_token = $3, lease_expires_at = $4, attempts = attempts + 1 FROM due WHERE e.id = due.id \
             RETURNING {}",
            COLUMNS.split(", ").map(|c| format!("e.{c}")).collect::<Vec<_>>().join(", ")
        ),
        vec![now.into(), batch.into(), token.into(), (now + chrono::Duration::minutes(10)).into()],
    )
    .await?;
    let mut done = 0;
    for e in due {
        let outcome = match build(state, &e, now).await {
            Ok((bytes, count)) => match state
                .storage
                .put(&e.object_key, bytes, "application/json")
                .await
            {
                Ok(stored) => Ok((stored, count)),
                Err(_) => Err("STORAGE_UNAVAILABLE"),
            },
            Err(r) => Err(r),
        };
        match outcome {
            Ok((stored, count)) => {
                let txn = sea_orm::TransactionTrait::begin(&state.db).await?;
                let ready = db::exec(
                    &txn,
                    "UPDATE messaging_exports SET status = 'READY', size = $3, sha256 = $4, message_count = $5, completed_at = $6, \
                       expires_at = $7, lease_token = NULL, lease_expires_at = NULL WHERE id = $1 AND lease_token = $2",
                    vec![
                        e.id.into(),
                        token.into(),
                        (stored.size as i64).into(),
                        stored.sha256.into(),
                        count.into(),
                        now.into(),
                        (now + chrono::Duration::days(DOWNLOAD_DAYS)).into(),
                    ],
                )
                .await?;
                if ready == 1 {
                    export_event(&txn, &e, "messaging.export.ready", "READY", None, now).await?;
                }
                txn.commit().await?;
                done += 1;
                crate::metrics::inc("messaging_exports_total", &[("outcome", "ready")]);
            }
            Err(reason) => {
                let permanent = matches!(reason, "TOO_LARGE" | "NOT_MANAGED" | "SEAL_UNREADABLE")
                    || e.attempts >= MAX_ATTEMPTS;
                let txn = sea_orm::TransactionTrait::begin(&state.db).await?;
                let settled = db::exec(
                    &txn,
                    "UPDATE messaging_exports SET status = CASE WHEN $3 THEN 'FAILED' ELSE 'PENDING' END, \
                       failure = $4, lease_token = NULL, lease_expires_at = NULL, \
                       completed_at = CASE WHEN $3 THEN $5 ELSE NULL END WHERE id = $1 AND lease_token = $2",
                    vec![e.id.into(), token.into(), permanent.into(), reason.into(), now.into()],
                )
                .await?;
                if permanent && settled == 1 {
                    export_event(
                        &txn,
                        &e,
                        "messaging.export.failed",
                        "FAILED",
                        Some(reason),
                        now,
                    )
                    .await?;
                }
                txn.commit().await?;
                tracing::warn!(export = %e.id, reason, permanent, "export failed");
                crate::metrics::inc(
                    "messaging_exports_total",
                    &[("outcome", if permanent { "failed" } else { "retry" })],
                );
            }
        }
    }
    Ok(done)
}

/// Expired READY exports: EXPIRED, object queued for deletion.
pub async fn expire(state: &AppState, batch: i64) -> Result<usize> {
    let now = state.now();
    let txn = sea_orm::TransactionTrait::begin(&state.db).await?;
    let due: Vec<ExportRow> = db::all(
        &txn,
        &format!(
            "UPDATE messaging_exports SET status = 'EXPIRED' WHERE id IN (SELECT id FROM messaging_exports WHERE status = 'READY' \
               AND expires_at <= $1 ORDER BY expires_at LIMIT $2 FOR UPDATE SKIP LOCKED) RETURNING {COLUMNS}"
        ),
        vec![now.into(), batch.into()],
    )
    .await?;
    for x in &due {
        enqueue_deletion(
            &txn,
            &x.object_key,
            x.environment_id,
            None,
            "EXPORT_EXPIRED",
            now,
        )
        .await?;
        export_event(&txn, x, "messaging.export.expired", "EXPIRED", None, now).await?;
    }
    txn.commit().await?;
    Ok(due.len())
}

/// Records an export transition (§118 export.ready; ids and codes only).
async fn export_event<C: sea_orm::ConnectionTrait>(
    db: &C,
    e: &ExportRow,
    event_type: &'static str,
    status: &str,
    failure: Option<&str>,
    now: DateTime<Utc>,
) -> Result<()> {
    crate::events::outbox::record(
        db,
        event_type,
        "EXPORT",
        e.id,
        crate::events::outbox::EventScope {
            tenant_id: e.tenant_id,
            application_id: e.application_id,
            environment_id: e.environment_id,
        },
        json!({ "export_id": e.id, "conversation_id": e.conversation_id, "status": status, "failure": failure }),
        now,
    )
    .await?;
    Ok(())
}
