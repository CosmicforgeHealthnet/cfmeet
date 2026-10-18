//! The PRIVATE_E2EE message path: opaque MLS ciphertext only.
//!
//! The server acts as the MLS Delivery Service: from the PUBLIC framing it checks that
//! the message belongs to this conversation's group, is an application message (commits
//! go through [`crate::mls`]) and was created in the group's CURRENT epoch; while a
//! removed / revoked device still awaits its removal commit, application messages are
//! refused (`MLS_REKEY_REQUIRED`) so nothing new is encrypted to a removed leaf. The
//! server then assigns the canonical sequence and binds sequence, id and scope to the
//! ciphertext digest in the acceptance record. It never decrypts, never parses content
//! and holds no group key.

use cf_messaging_crypto::framing::{self, FramedContent};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde_json::Value;
use uuid::Uuid;

use super::{
    decode_b64, load_by_client_id, load_message, record_created, render, MessageRow, SendRequest,
};
use crate::{
    conversations::{
        policy::EffectivePolicy,
        sequence::{allocate_sequence, AcceptanceRecord, AcceptanceTarget},
        service::{request_context, ConversationRow},
    },
    db,
    domain::profile::{OpaqueCiphertext, TransportMetadata},
    error::{Error, ErrorCode, Result},
    http::middleware::Acting,
    identity::TrustedContext,
    state::AppState,
};

/// Largest accepted MLS message (application or commit) in bytes.
pub const MAX_MLS_MESSAGE_BYTES: usize = 1024 * 1024;

/// The acting principal's ACTIVE device in this environment, or a refusal.
pub async fn require_own_device<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    principal: Uuid,
    device_id: Uuid,
) -> Result<()> {
    #[derive(FromQueryResult)]
    struct D {
        status: String,
    }
    let d: Option<D> = db::one(
        db,
        "SELECT status FROM messaging_devices WHERE id = $1 AND tenant_id = $2 AND application_id = $3 \
           AND environment_id = $4 AND principal_id = $5",
        vec![
            device_id.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            principal.into(),
        ],
    )
    .await?;
    match d {
        Some(d) if d.status == "ACTIVE" => Ok(()),
        Some(_) => Err(Error::code(ErrorCode::DeviceRevoked)),
        None => Err(Error::code(ErrorCode::NotFound)),
    }
}

/// The group's Delivery Service state, locked for this transaction.
#[derive(Debug, Clone, FromQueryResult)]
pub struct GroupState {
    pub mls_epoch: i64,
    pub rekey_required: bool,
}

pub async fn lock_group<C: ConnectionTrait>(db: &C, conversation_id: Uuid) -> Result<GroupState> {
    db::one(
        db,
        "SELECT mls_epoch, rekey_required FROM messaging_mls_groups WHERE conversation_id = $1 FOR UPDATE",
        vec![conversation_id.into()],
    )
    .await?
    .ok_or(Error::code(ErrorCode::InternalError))
}

/// Checks the PUBLIC framing of `bytes` against the conversation and the claimed epoch.
pub fn check_framing(
    conversation: &ConversationRow,
    bytes: &[u8],
    expected: FramedContent,
    epoch: u64,
) -> Result<()> {
    let f = framing::inspect(bytes)
        .map_err(|_| Error::reason(ErrorCode::ContentRejected, "NOT_AN_MLS_MESSAGE"))?;
    if Some(f.group_id.as_slice()) != conversation.mls_group_id.as_deref() {
        return Err(Error::reason(ErrorCode::ContentRejected, "WRONG_GROUP"));
    }
    if f.content != expected {
        return Err(Error::reason(
            ErrorCode::ContentRejected,
            match expected {
                FramedContent::Application => "NOT_AN_APPLICATION_MESSAGE",
                _ => "NOT_A_COMMIT",
            },
        ));
    }
    if f.epoch != epoch {
        return Err(Error::reason(
            ErrorCode::ContentRejected,
            "EPOCH_FIELD_MISMATCH",
        ));
    }
    Ok(())
}

/// One E2EE row to insert.
pub struct NewE2eeMessage<'a> {
    pub conversation: &'a ConversationRow,
    pub ctx: &'a TrustedContext,
    pub sender: Uuid,
    pub kind: &'static str,
    pub target: Option<Uuid>,
    pub metadata: TransportMetadata,
    pub ciphertext: &'a OpaqueCiphertext,
    pub attachment_count: i32,
}

/// Allocates the next sequence, computes the acceptance record and inserts the row.
pub async fn insert<C: ConnectionTrait>(
    db: &C,
    m: NewE2eeMessage<'_>,
    now: DateTime<Utc>,
) -> Result<MessageRow> {
    let sequence = allocate_sequence(db, m.conversation.id).await?;
    let id = Uuid::new_v4();
    let group: [u8; 32] = m
        .conversation
        .mls_group_id
        .clone()
        .and_then(|g| g.try_into().ok())
        .ok_or(Error::code(ErrorCode::InternalError))?;
    // Microsecond precision: what PostgreSQL stores is exactly what the digest binds.
    let accepted_at = DateTime::from_timestamp_micros(now.timestamp_micros()).unwrap_or(now);
    let record = AcceptanceRecord::new(
        AcceptanceTarget {
            tenant_id: m.ctx.scope.tenant_id,
            application_id: m.ctx.scope.application_id,
            environment_id: m.ctx.scope.environment_id,
            conversation_id: m.conversation.id,
            mls_group_id: group,
        },
        &m.metadata,
        m.ciphertext,
        sequence,
        id,
        accepted_at,
    );
    db::exec(
        db,
        "INSERT INTO messaging_messages (id, conversation_id, tenant_id, application_id, environment_id, sequence, \
           message_kind, security_profile, sender_principal_id, sender_device_id, client_message_id, target_message_id, \
           mls_epoch, mls_ciphertext, ciphertext_sha256, acceptance_digest, attachment_count, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'PRIVATE_E2EE', $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)",
        vec![
            id.into(),
            m.conversation.id.into(),
            m.ctx.scope.tenant_id.into(),
            m.ctx.scope.application_id.into(),
            m.ctx.scope.environment_id.into(),
            sequence.into(),
            m.kind.into(),
            m.sender.into(),
            m.metadata.sender_device_id.into(),
            m.metadata.client_message_id.into(),
            m.target.into(),
            (m.metadata.epoch as i64).into(),
            m.ciphertext.as_bytes().to_vec().into(),
            hex::encode(m.ciphertext.digest()).into(),
            hex::encode(record.digest()).into(),
            m.attachment_count.into(),
            accepted_at.into(),
        ],
    )
    .await?;
    load_message(db, m.conversation.id, id, false)
        .await?
        .ok_or(Error::code(ErrorCode::InternalError))
}

/// An idempotent retry: the same client message id with the same ciphertext returns the
/// stored message; with different bytes it is refused.
pub fn replayed(existing: &MessageRow, ciphertext: &OpaqueCiphertext) -> Result<()> {
    if existing.ciphertext_sha256.as_deref() == Some(hex::encode(ciphertext.digest()).as_str()) {
        Ok(())
    } else {
        Err(Error::code(ErrorCode::IdempotencyKeyReused))
    }
}

pub async fn send(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation: &ConversationRow,
    effective: &EffectivePolicy,
    request: SendRequest,
) -> Result<(u16, Value)> {
    if request.text.is_some() {
        // Never accept plaintext into an E2EE conversation (no downgrade).
        return Err(Error::reason(
            ErrorCode::ContentRejected,
            "PLAINTEXT_IN_E2EE",
        ));
    }
    let (Some(device_id), Some(epoch), Some(ciphertext)) = (
        request.device_id,
        request.epoch,
        request.ciphertext.as_deref(),
    ) else {
        return Err(Error::validation(
            "PRIVATE_E2EE messages need deviceId, epoch and ciphertext",
        ));
    };
    let kind = match request.kind.as_deref() {
        None | Some("APPLICATION") => "APPLICATION",
        Some("EDIT") => "EDIT",
        Some(_) => return Err(Error::validation("kind must be APPLICATION or EDIT")),
    };
    if (kind == "EDIT") != request.target_message_id.is_some() {
        return Err(Error::validation(
            "targetMessageId is required for (and only for) EDIT",
        ));
    }
    let bytes = decode_b64("ciphertext", ciphertext, MAX_MLS_MESSAGE_BYTES)?;
    let ciphertext = OpaqueCiphertext::new(bytes)
        .map_err(|_| Error::reason(ErrorCode::ContentRejected, "SIZE"))?;
    let metadata = TransportMetadata {
        client_message_id: request.client_message_id,
        sender_device_id: device_id,
        epoch,
    };
    metadata
        .validate()
        .map_err(|_| Error::validation("invalid transport metadata"))?;
    require_own_device(&state.db, ctx, acting.principal_id, device_id).await?;
    check_framing(
        conversation,
        ciphertext.as_bytes(),
        FramedContent::Application,
        epoch,
    )?;
    if let Some(existing) = load_by_client_id(
        &state.db,
        conversation.id,
        acting.principal_id,
        request.client_message_id,
    )
    .await?
    {
        replayed(&existing, &ciphertext)?;
        return Ok((200, render(state, &existing, false).await));
    }
    let st = state.clone();
    let ctx = ctx.clone();
    let acting = acting.clone();
    let conversation = conversation.clone();
    let edit_window = effective.editing.edit_window_seconds;
    let request_ctx = request_context();
    let attachments = request.attachment_ids.clone();
    let target = request.target_message_id;
    let row = state
        .db
        .transaction::<_, (u16, MessageRow), Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                super::lock_sender(txn, &conversation, acting.principal_id, now).await?;
                let group = lock_group(txn, conversation.id).await?;
                if let Some(existing) = load_by_client_id(
                    txn,
                    conversation.id,
                    acting.principal_id,
                    metadata.client_message_id,
                )
                .await?
                {
                    replayed(&existing, &ciphertext)?;
                    return Ok((200, existing));
                }
                if group.rekey_required {
                    return Err(Error::code(ErrorCode::MlsRekeyRequired));
                }
                if group.mls_epoch as u64 != epoch {
                    return Err(Error::code(ErrorCode::EpochMismatch));
                }
                if let Some(t) = target {
                    edit_target(txn, &conversation, t, acting.principal_id, edit_window, now)
                        .await?;
                }
                let row = insert(
                    txn,
                    NewE2eeMessage {
                        conversation: &conversation,
                        ctx: &ctx,
                        sender: acting.principal_id,
                        kind,
                        target,
                        metadata,
                        ciphertext: &ciphertext,
                        attachment_count: attachments.len() as i32,
                    },
                    now,
                )
                .await?;
                crate::attachments::link_to_message(
                    txn,
                    &conversation,
                    row.id,
                    acting.principal_id,
                    &attachments,
                )
                .await?;
                record_created(txn, &ctx, &row, &request_ctx, now).await?;
                Ok((201, row))
            })
        })
        .await
        .map_err(Error::from)?;
    Ok((row.0, render(state, &row.1, false).await))
}

/// Validates and marks the target of an edit (own, ACTIVE, application message, within
/// the policy's window).
pub(crate) async fn edit_target<C: ConnectionTrait>(
    db: &C,
    conversation: &ConversationRow,
    target: Uuid,
    sender: Uuid,
    window: Option<i64>,
    now: DateTime<Utc>,
) -> Result<()> {
    let t = load_message(db, conversation.id, target, true)
        .await?
        .ok_or(Error::code(ErrorCode::NotFound))?;
    if t.sender_principal_id != sender {
        return Err(Error::reason(
            ErrorCode::PrincipalNotAuthorized,
            "NOT_SENDER",
        ));
    }
    if t.message_kind != "APPLICATION" || !t.is_active() {
        return Err(Error::reason(
            ErrorCode::MessageNotModifiable,
            "NOT_EDITABLE",
        ));
    }
    if window.is_some_and(|w| now > t.created_at + chrono::Duration::seconds(w)) {
        return Err(Error::reason(
            ErrorCode::MessageNotModifiable,
            "WINDOW_CLOSED",
        ));
    }
    db::exec(
        db,
        "UPDATE messaging_messages SET edit_count = edit_count + 1, edited_at = $2 WHERE id = $1",
        vec![t.id.into(), now.into()],
    )
    .await?;
    Ok(())
}
