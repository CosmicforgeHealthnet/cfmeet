//! Legal and compliance holds (§91).
//!
//! * Created, listed and released only by the application's administration
//!   (`conversations.manage`, no acting principal); every operation is audited.
//! * Scope: one CONVERSATION, one PRINCIPAL (their messages and uploads) or the whole
//!   ENVIRONMENT. An optional expiry ends a hold automatically.
//! * Every purge / object deletion asks [`message_held`] / [`attachment_held`] first: a
//!   held item is preserved (deletion queue `HELD`), never removed.
//! * Hold creation and destruction are serialised per environment ([`lock_for_destruction`]
//!   shared, hold creation exclusive): a purge that checked "not held" finishes before a
//!   concurrent hold is acknowledged, and a purge that starts after the acknowledgement
//!   sees the hold. An acknowledged hold therefore never loses data to an in-flight purge.
//! * PRIVATE_E2EE: a hold preserves ciphertext, encrypted attachment objects, metadata and
//!   voluntarily disclosed report evidence — and NOTHING more. It never creates a way to
//!   decrypt: content that must be readable by the server belongs in MANAGED from the
//!   conversation's creation (§92).

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    audit::{self, AuditEntry},
    conversations::service::{request_context, rfc3339},
    db,
    error::{Error, ErrorCode, Result},
    identity::TrustedContext,
    state::AppState,
};

/// Advisory-lock key of an environment's hold / destruction serialisation.
const LOCK_KEY: &str = "hashtextextended('messaging-legal-hold:' || $1::text, 0)";

/// Taken (shared, transaction-scoped) by every purge / deletion BEFORE it checks for a
/// hold, and held until its transaction ends. MUST run inside a transaction.
pub async fn lock_for_destruction<C: ConnectionTrait>(db: &C, environment_id: Uuid) -> Result<()> {
    db::exec(
        db,
        &format!("SELECT pg_advisory_xact_lock_shared({LOCK_KEY})"),
        vec![environment_id.into()],
    )
    .await?;
    Ok(())
}

/// Taken (exclusive, transaction-scoped) by hold creation: waits for in-flight purges of
/// the environment, and blocks new ones until the hold is committed.
async fn lock_for_new_hold<C: ConnectionTrait>(db: &C, environment_id: Uuid) -> Result<()> {
    db::exec(
        db,
        &format!("SELECT pg_advisory_xact_lock({LOCK_KEY})"),
        vec![environment_id.into()],
    )
    .await?;
    Ok(())
}

const ACTIVE: &str = "h.status = 'ACTIVE' AND (h.expires_at IS NULL OR h.expires_at > $2)";

/// Whether an active hold protects a message.
pub async fn message_held<C: ConnectionTrait>(
    db: &C,
    message_id: Uuid,
    now: DateTime<Utc>,
) -> Result<bool> {
    Ok(db::scalar_i64(
        db,
        &format!(
            "SELECT count(*) FROM messaging_messages m JOIN messaging_legal_holds h ON h.environment_id = m.environment_id \
              WHERE m.id = $1 AND {ACTIVE} AND (h.scope_kind = 'ENVIRONMENT' \
                OR (h.scope_kind = 'CONVERSATION' AND h.conversation_id = m.conversation_id) \
                OR (h.scope_kind = 'PRINCIPAL' AND h.principal_id = m.sender_principal_id))"
        ),
        vec![message_id.into(), now.into()],
    )
    .await?
        > 0)
}

/// Whether an active hold protects an attachment (its conversation, its uploader, its
/// message's sender, or the environment).
pub async fn attachment_held<C: ConnectionTrait>(
    db: &C,
    attachment_id: Uuid,
    now: DateTime<Utc>,
) -> Result<bool> {
    Ok(db::scalar_i64(
        db,
        &format!(
            "SELECT count(*) FROM messaging_attachments a JOIN messaging_legal_holds h ON h.environment_id = a.environment_id \
              WHERE a.id = $1 AND {ACTIVE} AND (h.scope_kind = 'ENVIRONMENT' \
                OR (h.scope_kind = 'CONVERSATION' AND h.conversation_id = a.conversation_id) \
                OR (h.scope_kind = 'PRINCIPAL' AND h.principal_id = a.uploader_principal_id))"
        ),
        vec![attachment_id.into(), now.into()],
    )
    .await?
        > 0)
}

#[derive(Debug, Clone, FromQueryResult)]
struct HoldRow {
    id: Uuid,
    scope_kind: String,
    conversation_id: Option<Uuid>,
    principal_id: Option<Uuid>,
    reason_category: String,
    case_reference: Option<String>,
    status: String,
    created_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
    released_at: Option<DateTime<Utc>>,
    release_reason: Option<String>,
}

const COLUMNS: &str = "id, scope_kind, conversation_id, principal_id, reason_category, case_reference, status, created_at, \
     expires_at, released_at, release_reason";

fn view(h: &HoldRow, now: DateTime<Utc>) -> Value {
    let effective = h.status == "ACTIVE" && h.expires_at.is_none_or(|t| t > now);
    json!({
        "holdId": h.id,
        "scope": { "kind": h.scope_kind, "conversationId": h.conversation_id, "principalId": h.principal_id },
        "reasonCategory": h.reason_category,
        "caseReference": h.case_reference,
        "status": h.status,
        "effective": effective,
        "preserves": "MESSAGES_ATTACHMENTS_AND_METADATA_AS_STORED",
        "decryptionCapability": "NONE",
        "createdAt": rfc3339(h.created_at),
        "expiresAt": h.expires_at.map(rfc3339),
        "releasedAt": h.released_at.map(rfc3339),
        "releaseReason": h.release_reason,
    })
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateHold {
    pub scope_kind: String,
    #[serde(default)]
    pub conversation_id: Option<Uuid>,
    #[serde(default)]
    pub principal_id: Option<Uuid>,
    pub reason_category: String,
    #[serde(default)]
    pub case_reference: Option<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

pub async fn create(state: &AppState, ctx: &TrustedContext, request: CreateHold) -> Result<Value> {
    let now = state.now();
    if !matches!(
        request.reason_category.as_str(),
        "LITIGATION" | "REGULATORY" | "INVESTIGATION" | "INTERNAL_POLICY"
    ) {
        return Err(Error::validation(
            "reasonCategory must be LITIGATION, REGULATORY, INVESTIGATION or INTERNAL_POLICY",
        ));
    }
    if request.case_reference.as_deref().is_some_and(|c| {
        c.is_empty()
            || c.len() > 128
            || !c
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".:_/-".contains(&b))
    }) {
        return Err(Error::validation(
            "caseReference must be 1-128 characters of [A-Za-z0-9._:/-]",
        ));
    }
    if request.expires_at.is_some_and(|t| t <= now) {
        return Err(Error::validation("expiresAt must be in the future"));
    }
    match (request.scope_kind.as_str(), request.conversation_id, request.principal_id) {
        ("CONVERSATION", Some(c), None) => {
            crate::conversations::access::in_scope(&state.db, ctx, c).await?;
        }
        ("PRINCIPAL", None, Some(_)) | ("ENVIRONMENT", None, None) => {}
        _ => {
            return Err(Error::validation(
                "scopeKind CONVERSATION needs conversationId, PRINCIPAL needs principalId, ENVIRONMENT neither",
            ))
        }
    }
    let id = Uuid::new_v4();
    let txn = state.db.begin().await?;
    lock_for_new_hold(&txn, ctx.scope.environment_id).await?;
    db::exec(
        &txn,
        "INSERT INTO messaging_legal_holds (id, tenant_id, application_id, environment_id, scope_kind, conversation_id, principal_id, \
           reason_category, case_reference, created_by_credential_id, created_at, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        vec![
            id.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            request.scope_kind.clone().into(),
            request.conversation_id.into(),
            request.principal_id.into(),
            request.reason_category.clone().into(),
            request.case_reference.clone().into(),
            ctx.credential_id.into(),
            now.into(),
            request.expires_at.into(),
        ],
    )
    .await?;
    crate::events::outbox::record(
        &txn,
        "messaging.legal-hold.applied",
        "LEGAL_HOLD",
        id,
        crate::conversations::service::scope_of(ctx),
        json!({ "hold_id": id, "scope_kind": request.scope_kind, "conversation_id": request.conversation_id,
                "principal_id": request.principal_id, "reason_category": request.reason_category }),
        now,
    )
    .await?;
    txn.commit().await?;
    audit::record(
        &state.db,
        AuditEntry {
            ctx,
            principal_id: None,
            action: "LEGAL_HOLD_CREATED",
            target_kind: "LEGAL_HOLD",
            target_id: id,
            detail: json!({ "scope_kind": request.scope_kind, "reason_category": request.reason_category, "outcome": "APPLIED" }),
        },
        &request_context(),
        now,
    )
    .await?;
    crate::metrics::inc("messaging_legal_holds_created_total", &[]);
    get(state, ctx, id).await
}

async fn load<C: ConnectionTrait>(db: &C, ctx: &TrustedContext, id: Uuid) -> Result<HoldRow> {
    db::one(
        db,
        &format!("SELECT {COLUMNS} FROM messaging_legal_holds WHERE id = $1 AND environment_id = $2 AND tenant_id = $3"),
        vec![id.into(), ctx.scope.environment_id.into(), ctx.scope.tenant_id.into()],
    )
    .await?
    .ok_or(Error::code(ErrorCode::NotFound))
}

pub async fn get(state: &AppState, ctx: &TrustedContext, id: Uuid) -> Result<Value> {
    Ok(view(&load(&state.db, ctx, id).await?, state.now()))
}

pub async fn list(state: &AppState, ctx: &TrustedContext) -> Result<Value> {
    let rows: Vec<HoldRow> = db::all(
        &state.db,
        &format!("SELECT {COLUMNS} FROM messaging_legal_holds WHERE environment_id = $1 AND tenant_id = $2 ORDER BY created_at DESC LIMIT 500"),
        vec![ctx.scope.environment_id.into(), ctx.scope.tenant_id.into()],
    )
    .await?;
    let now = state.now();
    Ok(json!({ "holds": rows.iter().map(|h| view(h, now)).collect::<Vec<_>>() }))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseHold {
    pub reason: String,
}

pub async fn release(
    state: &AppState,
    ctx: &TrustedContext,
    id: Uuid,
    request: ReleaseHold,
) -> Result<Value> {
    if !crate::membership::valid_reason(&request.reason) {
        return Err(Error::validation(
            "reason must be an UPPER_SNAKE_CASE code of 2-48 characters",
        ));
    }
    let now = state.now();
    let hold = load(&state.db, ctx, id).await?;
    let txn = state.db.begin().await?;
    let n = db::exec(
        &txn,
        "UPDATE messaging_legal_holds SET status = 'RELEASED', released_at = $3, released_by_credential_id = $4, release_reason = $5 \
         WHERE id = $1 AND environment_id = $2 AND status = 'ACTIVE'",
        vec![id.into(), ctx.scope.environment_id.into(), now.into(), ctx.credential_id.into(), request.reason.clone().into()],
    )
    .await?;
    if n == 1 {
        // Deletions held by this hold are re-evaluated at once.
        db::exec(
            &txn,
            "UPDATE messaging_object_deletions SET status = 'PENDING', next_attempt_at = $2 WHERE environment_id = $1 AND status = 'HELD'",
            vec![ctx.scope.environment_id.into(), now.into()],
        )
        .await?;
        crate::events::outbox::record(
            &txn,
            "messaging.legal-hold.released",
            "LEGAL_HOLD",
            id,
            crate::conversations::service::scope_of(ctx),
            json!({ "hold_id": id, "scope_kind": hold.scope_kind, "conversation_id": hold.conversation_id,
                    "principal_id": hold.principal_id, "reason": request.reason }),
            now,
        )
        .await?;
    }
    txn.commit().await?;
    if n == 1 {
        audit::record(
            &state.db,
            AuditEntry {
                ctx,
                principal_id: None,
                action: "LEGAL_HOLD_RELEASED",
                target_kind: "LEGAL_HOLD",
                target_id: id,
                detail: json!({ "reason": request.reason, "outcome": "RELEASED" }),
            },
            &request_context(),
            now,
        )
        .await?;
    }
    get(state, ctx, id).await
}
