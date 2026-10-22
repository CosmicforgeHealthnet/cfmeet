//! The MANAGED message store (Part V §2.1 MANAGED, Appendix J): server-processable text
//! for conversations whose persisted profile is MANAGED. NEVER reachable from a
//! PRIVATE_E2EE path (repository guard `managed_content_never_enters_e2ee_paths`): text
//! is admitted only through [`MessagePayload::accept`] with the conversation's
//! persisted profile.
//!
//! * At rest the text is sealed (XChaCha20-Poly1305, random 192-bit nonce) under the
//!   deployment's managed key ring ([`ManagedKeyring`]); the associated data binds tenant,
//!   application, environment, conversation and message id, so a sealed value cannot be
//!   moved to another message. Key rotation: new messages use the active key; old ones
//!   keep their key id.
//! * For authorized MANAGED search a PostgreSQL `tsvector` (language-neutral `simple`
//!   configuration) is stored beside it. That index holds normalized words in the clear
//!   inside Messaging's database — a disclosed property of MANAGED, never of E2EE.
//! * Without a configured key ring MANAGED sending fails closed.
//!
//! TLS and database encryption are not end-to-end encryption; MANAGED is never
//! described as E2EE.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde_json::Value;
use uuid::Uuid;

use super::{load_by_client_id, load_message, render, MessageRow, SendRequest};
use crate::{
    conversations::{
        policy::EffectivePolicy,
        sequence::allocate_sequence,
        service::{request_context, scope_of, ConversationRow},
    },
    db,
    domain::{
        profile::{ManagedContent, MessagePayload, SubmittedContent},
        SecurityProfile,
    },
    error::{Error, ErrorCode, Result},
    events::outbox::{self, NewEvent},
    http::middleware::Acting,
    identity::TrustedContext,
    limits::{self, Window},
    state::AppState,
};

use super::keys::SealScope;
pub use super::keys::{DecryptPurpose, ManagedKeyring};

fn scope_of_row(row: &MessageRow) -> SealScope {
    SealScope {
        tenant_id: row.tenant_id,
        application_id: row.application_id,
        environment_id: row.environment_id,
        conversation_id: row.conversation_id,
        message_id: row.id,
    }
}

/// The text of an ACTIVE MANAGED message (`None` for any other row or when the key ring
/// cannot open it), opened for `purpose`.
pub async fn text_of(
    state: &AppState,
    row: &MessageRow,
    purpose: DecryptPurpose,
) -> Option<String> {
    if row.profile() != SecurityProfile::Managed || !row.is_active() {
        return None;
    }
    let ring = state.managed_keys.as_ref()?;
    ring.open(
        &state.db,
        &scope_of_row(row),
        row.managed_sealed.as_deref()?,
        row.managed_seal_nonce.as_deref()?,
        row.managed_seal_key_id.as_deref()?,
        purpose,
    )
    .await
}

/// The stored text of a MANAGED message whatever its lifecycle (moderation review of
/// reported content; `None` once purged). Never used for member-facing reads.
pub async fn stored_text_of(state: &AppState, row: &MessageRow) -> Option<String> {
    if row.profile() != SecurityProfile::Managed {
        return None;
    }
    state
        .managed_keys
        .as_ref()?
        .open(
            &state.db,
            &scope_of_row(row),
            row.managed_sealed.as_deref()?,
            row.managed_seal_nonce.as_deref()?,
            row.managed_seal_key_id.as_deref()?,
            DecryptPurpose::ModerationReview,
        )
        .await
}

pub(crate) fn admit(text: String) -> Result<ManagedContent> {
    match MessagePayload::accept(
        SecurityProfile::Managed,
        SubmittedContent::ManagedText(text),
    ) {
        Ok(MessagePayload::Managed(c)) => Ok(c),
        _ => Err(Error::reason(ErrorCode::ContentRejected, "MANAGED_TEXT")),
    }
}

fn ring(state: &AppState) -> Result<&ManagedKeyring> {
    state.managed_keys.as_deref().ok_or(Error::reason(
        ErrorCode::FeatureDisabled,
        "MANAGED_STORAGE_NOT_CONFIGURED",
    ))
}

/// Counts links of MANAGED text against the abuse policy (content is visible by design
/// in MANAGED; nothing of the kind exists for E2EE).
async fn link_limit(
    state: &AppState,
    ctx: &TrustedContext,
    principal: Uuid,
    text: &str,
    limit: i64,
) -> Result<()> {
    let links = text.matches("http://").count() + text.matches("https://").count();
    let key = format!(
        "links:{}:{}",
        ctx.scope.environment_id.simple(),
        principal.simple()
    );
    for _ in 0..links.min(20) {
        if let Some(retry) = limits::hit(&state.db, &key, Window::Hour, limit, state.now()).await? {
            crate::safety::abuse::signal(state, ctx, principal, "MANAGED_LINK_VELOCITY").await;
            return Err(Error::RateLimited {
                retry_after_seconds: retry,
            });
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn insert<C: ConnectionTrait>(
    db: &C,
    ring: &ManagedKeyring,
    pool: &sea_orm::DatabaseConnection,
    ctx: &TrustedContext,
    conversation: &ConversationRow,
    sender: Uuid,
    client_message_id: Uuid,
    content: &ManagedContent,
    attachment_count: i32,
    presentation: Option<&str>,
    now: DateTime<Utc>,
) -> Result<MessageRow> {
    let sequence = allocate_sequence(db, conversation.id).await?;
    let id = Uuid::new_v4();
    let sealed = ring
        .seal(
            pool,
            &SealScope {
                tenant_id: ctx.scope.tenant_id,
                application_id: ctx.scope.application_id,
                environment_id: ctx.scope.environment_id,
                conversation_id: conversation.id,
                message_id: id,
            },
            content.text(),
        )
        .await?;
    db::exec(
        db,
        "INSERT INTO messaging_messages (id, conversation_id, tenant_id, application_id, environment_id, sequence, \
           message_kind, security_profile, sender_principal_id, client_message_id, managed_sealed, managed_seal_nonce, \
           managed_seal_key_id, managed_search, attachment_count, created_at, managed_presentation) \
         VALUES ($1, $2, $3, $4, $5, $6, 'APPLICATION', 'MANAGED', $7, $8, $9, $10, $11, to_tsvector('simple', $12), $13, $14, $15)",
        vec![
            id.into(),
            conversation.id.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            sequence.into(),
            sender.into(),
            client_message_id.into(),
            sealed.sealed.into(),
            sealed.nonce.into(),
            sealed.key_id.into(),
            content.text().to_string().into(),
            attachment_count.into(),
            now.into(),
            presentation.map(str::to_string).into(),
        ],
    )
    .await?;
    load_message(db, conversation.id, id, false)
        .await?
        .ok_or(Error::code(ErrorCode::InternalError))
}

/// A MANAGED message that arrived on another channel (the SMS Bridge): stored like any
/// MANAGED message, labelled with its channel.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn insert_channel<C: ConnectionTrait>(
    db: &C,
    ring: &ManagedKeyring,
    pool: &sea_orm::DatabaseConnection,
    ctx: &TrustedContext,
    conversation: &ConversationRow,
    sender: Uuid,
    client_message_id: Uuid,
    content: &ManagedContent,
    channel: &'static str,
    now: DateTime<Utc>,
) -> Result<MessageRow> {
    let row = insert(
        db,
        ring,
        pool,
        ctx,
        conversation,
        sender,
        client_message_id,
        content,
        0,
        None,
        now,
    )
    .await?;
    db::exec(
        db,
        "UPDATE messaging_messages SET channel = $2 WHERE id = $1",
        vec![row.id.into(), channel.into()],
    )
    .await?;
    load_message(db, conversation.id, row.id, false)
        .await?
        .ok_or(Error::code(ErrorCode::InternalError))
}

async fn same_text(state: &AppState, existing: &MessageRow, text: &str) -> Result<()> {
    match text_of(state, existing, DecryptPurpose::Idempotency).await {
        Some(t) if t == text => Ok(()),
        // A deleted / purged original cannot be compared: a retry of a deleted message
        // is answered with its tombstone.
        None if !existing.is_active() => Ok(()),
        _ => Err(Error::code(ErrorCode::IdempotencyKeyReused)),
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
    let threads = effective.messages.threads;
    crate::operations::require_open(
        &state.db,
        crate::operations::KillSwitch::MessageSend,
        ctx.scope.environment_id,
    )
    .await?;
    crate::operations::require_open(
        &state.db,
        crate::operations::KillSwitch::ManagedMessaging,
        ctx.scope.environment_id,
    )
    .await?;

    if request.ciphertext.is_some() || request.device_id.is_some() || request.epoch.is_some() {
        return Err(Error::reason(
            ErrorCode::ContentRejected,
            "CIPHERTEXT_IN_MANAGED",
        ));
    }
    if request.kind.as_deref().is_some_and(|k| k != "APPLICATION")
        || request.target_message_id.is_some()
    {
        return Err(Error::validation("MANAGED edits use PATCH on the message"));
    }
    let presentation = match request.managed_presentation.as_deref() {
        None | Some("STANDARD") => None,
        Some("ANNOUNCEMENT") => Some("ANNOUNCEMENT"),
        Some(_) => {
            return Err(Error::validation(
                "managedPresentation must be STANDARD or ANNOUNCEMENT",
            ))
        }
    };
    let notify = request.notify.unwrap_or(true);
    let reply_to = request.reply_to_message_id;
    let text = request
        .text
        .ok_or_else(|| Error::validation("MANAGED messages need text"))?;
    let content = admit(text)?;
    let ring = ring(state)?;
    if let Some(existing) = load_by_client_id(
        &state.db,
        conversation.id,
        acting.principal_id,
        request.client_message_id,
    )
    .await?
    {
        same_text(state, &existing, content.text()).await?;
        return Ok((200, render(state, &existing, false).await));
    }
    link_limit(
        state,
        ctx,
        acting.principal_id,
        content.text(),
        effective.abuse.managed_links_per_hour,
    )
    .await?;
    let st = state.clone();
    let ctx2 = ctx.clone();
    let acting = acting.clone();
    let conversation = conversation.clone();
    let request_ctx = request_context();
    let attachments = request.attachment_ids.clone();
    let client_id = request.client_message_id;
    let _ = ring;
    let (status, row) = state
        .db
        .transaction::<_, (u16, MessageRow), Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let ring = st.managed_keys.as_deref().ok_or(Error::code(ErrorCode::FeatureDisabled))?;
                super::lock_sender(txn, &conversation, acting.principal_id, now, true).await?;
                // Serialize senders of this conversation's sequence before the retry check.
                db::exec(
                    txn,
                    "SELECT 1 FROM messaging_conversation_sequences WHERE conversation_id = $1 FOR UPDATE",
                    vec![conversation.id.into()],
                )
                .await?;
                if let Some(existing) = load_by_client_id(txn, conversation.id, acting.principal_id, client_id).await? {
                    return Ok((200, existing));
                }
                let row = insert(
                    txn,
                    ring,
                    &st.db,
                    &ctx2,
                    &conversation,
                    acting.principal_id,
                    client_id,
                    &content,
                    attachments.len() as i32,
                    presentation,
                    now,
                )
                .await?;
                crate::attachments::link_to_message(txn, &conversation, row.id, acting.principal_id, &attachments)
                    .await?;
                let row = crate::engagement::set_reply(txn, &conversation, row, reply_to, threads).await?;
                super::record_created_notify(txn, &ctx2, &row, &request_ctx, now, notify).await?;
                // A bridged conversation: queue the SMS copy in the same transaction.
                crate::sms_bridge::enqueue_outbound(txn, &conversation, &row, now).await?;
                Ok((201, row))
            })
        })
        .await
        .map_err(Error::from)?;
    Ok((status, render(state, &row, false).await))
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditRequest {
    pub text: String,
    /// Optimistic version check (§155): the `editCount` the editor saw. A concurrent
    /// edit that committed first makes this one `409 VERSION_CONFLICT`.
    #[serde(default)]
    pub expected_edit_count: Option<i32>,
}

/// Edits a MANAGED message (sender only, within the policy's window). The superseded
/// version is first kept as a sealed revision (§46: edits never overwrite evidence in
/// place), then the current text and the search index are replaced together, and
/// `message.edited` is recorded in the same transaction.
pub async fn edit(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    message_id: Uuid,
    request: EditRequest,
) -> Result<Value> {
    let (conversation, member) = crate::conversations::access::as_member(
        &state.db,
        ctx,
        conversation_id,
        acting.principal_id,
    )
    .await?;
    if conversation.profile() != SecurityProfile::Managed {
        return Err(Error::reason(
            ErrorCode::ContentRejected,
            "E2EE_EDITS_ARE_CIPHERTEXT",
        ));
    }
    crate::conversations::access::require_accepting(&conversation, state.now())?;
    let effective =
        crate::conversations::policy::current(&state.db, ctx.scope.environment_id).await?;
    crate::operations::require_open(
        &state.db,
        crate::operations::KillSwitch::ManagedMessaging,
        ctx.scope.environment_id,
    )
    .await?;
    let expected_edit_count = request.expected_edit_count;
    let content = admit(request.text)?;
    ring(state)?;
    let st = state.clone();
    let ctx2 = ctx.clone();
    let acting = acting.clone();
    let request_ctx = request_context();
    let row = state
        .db
        .transaction::<_, MessageRow, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let ring = st.managed_keys.as_deref().ok_or(Error::code(ErrorCode::FeatureDisabled))?;
                super::lock_sender(txn, &conversation, acting.principal_id, now, true).await?;
                let row = load_message(txn, conversation.id, message_id, true)
                    .await?
                    .filter(|r| r.sequence >= member.visible_from_sequence)
                    .ok_or(Error::code(ErrorCode::NotFound))?;
                if row.sender_principal_id != acting.principal_id {
                    return Err(Error::reason(ErrorCode::PrincipalNotAuthorized, "NOT_SENDER"));
                }
                if !row.is_active() {
                    return Err(Error::reason(ErrorCode::MessageNotModifiable, "NOT_ACTIVE"));
                }
                if effective
                    .editing
                    .edit_window_seconds
                    .is_some_and(|w| now > row.created_at + chrono::Duration::seconds(w))
                {
                    return Err(Error::reason(ErrorCode::MessageNotModifiable, "WINDOW_CLOSED"));
                }
                if expected_edit_count.is_some_and(|expected| expected != row.edit_count)
                {
                    return Err(Error::reason(ErrorCode::VersionConflict, "EDITED_CONCURRENTLY"));
                }
                keep_revision(txn, &row, acting.principal_id, now).await?;
                let sealed = ring.seal(&st.db, &scope_of_row(&row), content.text()).await?;
                db::exec(
                    txn,
                    "UPDATE messaging_messages SET managed_sealed = $2, managed_seal_nonce = $3, managed_seal_key_id = $4, \
                       managed_search = to_tsvector('simple', $5), edit_count = edit_count + 1, edited_at = $6 \
                     WHERE id = $1 AND lifecycle = 'ACTIVE'",
                    vec![
                        row.id.into(),
                        sealed.sealed.into(),
                        sealed.nonce.into(),
                        sealed.key_id.into(),
                        content.text().to_string().into(),
                        now.into(),
                    ],
                )
                .await?;
                outbox::enqueue_conversation(
                    txn,
                    conversation.id,
                    NewEvent {
                        event_type: "messaging.message.edited",
                        aggregate_type: "CONVERSATION",
                        aggregate_id: conversation.id,
                        scope: scope_of(&ctx2),
                        payload: serde_json::json!({
                            "message_id": row.id,
                            "target_message_id": row.id,
                            "sequence": row.sequence,
                            "sender_principal_id": row.sender_principal_id,
                        }),
                    },
                    &request_ctx,
                    now,
                )
                .await?;
                load_message(txn, conversation.id, row.id, false)
                    .await?
                    .ok_or(Error::code(ErrorCode::InternalError))
            })
        })
        .await
        .map_err(Error::from)?;
    Ok(render(state, &row, false).await)
}

/// Copies the version a MANAGED edit supersedes into `messaging_message_revisions`
/// (still sealed under the message's own scope). Revision `n` is the text as it stood
/// after `n` edits.
async fn keep_revision<C: ConnectionTrait>(
    db: &C,
    row: &MessageRow,
    edited_by: Uuid,
    now: DateTime<Utc>,
) -> Result<()> {
    let (Some(sealed), Some(nonce), Some(key_id)) = (
        row.managed_sealed.clone(),
        row.managed_seal_nonce.clone(),
        row.managed_seal_key_id.clone(),
    ) else {
        return Err(Error::code(ErrorCode::InternalError));
    };
    db::exec(
        db,
        "INSERT INTO messaging_message_revisions (message_id, revision, environment_id, conversation_id, \
           edited_by_principal_id, managed_sealed, managed_seal_nonce, managed_seal_key_id, created_at, superseded_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        vec![
            row.id.into(),
            row.edit_count.into(),
            row.environment_id.into(),
            row.conversation_id.into(),
            edited_by.into(),
            sealed.into(),
            nonce.into(),
            key_id.into(),
            row.edited_at.unwrap_or(row.created_at).into(),
            now.into(),
        ],
    )
    .await?;
    Ok(())
}

/// One kept revision of a MANAGED message, opened with the message's seal scope.
#[derive(Debug, Clone)]
pub struct Revision {
    pub revision: i32,
    pub text: Option<String>,
    pub created_at: DateTime<Utc>,
    pub superseded_at: DateTime<Utc>,
}

#[derive(Debug, FromQueryResult)]
struct RevisionRow {
    revision: i32,
    managed_sealed: Vec<u8>,
    managed_seal_nonce: Vec<u8>,
    managed_seal_key_id: String,
    created_at: DateTime<Utc>,
    superseded_at: DateTime<Utc>,
}

/// The kept revisions of a MANAGED message, oldest first (empty for PRIVATE_E2EE).
pub async fn revisions_of<C: ConnectionTrait>(
    state: &AppState,
    db: &C,
    row: &MessageRow,
    purpose: DecryptPurpose,
) -> Result<Vec<Revision>> {
    if row.profile() != SecurityProfile::Managed {
        return Ok(Vec::new());
    }
    let rows: Vec<RevisionRow> = db::all(
        db,
        "SELECT revision, managed_sealed, managed_seal_nonce, managed_seal_key_id, created_at, superseded_at \
           FROM messaging_message_revisions WHERE message_id = $1 ORDER BY revision",
        vec![row.id.into()],
    )
    .await?;
    let scope = scope_of_row(row);
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let text = match state.managed_keys.as_ref() {
            Some(ring) => {
                ring.open(
                    &state.db,
                    &scope,
                    &r.managed_sealed,
                    &r.managed_seal_nonce,
                    &r.managed_seal_key_id,
                    purpose,
                )
                .await
            }
            None => None,
        };
        out.push(Revision {
            revision: r.revision,
            text,
            created_at: r.created_at,
            superseded_at: r.superseded_at,
        });
    }
    Ok(out)
}

/// `GET /v1/conversations/{id}/messages/{messageId}/revisions` — the kept prior
/// versions of a MANAGED message, for members, only when the tenant's policy makes
/// revision history visible (§46). A deleted message shows no content.
pub async fn list_revisions(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    message_id: Uuid,
) -> Result<Value> {
    let (conversation, member) = crate::conversations::access::as_member(
        &state.db,
        ctx,
        conversation_id,
        acting.principal_id,
    )
    .await?;
    if conversation.profile() != SecurityProfile::Managed {
        return Err(Error::reason(
            ErrorCode::ContentRejected,
            "E2EE_EDITS_ARE_CIPHERTEXT",
        ));
    }
    let effective =
        crate::conversations::policy::current(&state.db, ctx.scope.environment_id).await?;
    if !effective.editing.revision_history_visible {
        return Err(Error::reason(
            ErrorCode::FeatureDisabled,
            "REVISION_HISTORY_HIDDEN",
        ));
    }
    let row = load_message(&state.db, conversation.id, message_id, false)
        .await?
        .filter(|r| r.sequence >= member.visible_from_sequence)
        .ok_or(Error::code(ErrorCode::NotFound))?;
    let revisions = if row.is_active() {
        revisions_of(state, &state.db, &row, DecryptPurpose::RevisionHistory).await?
    } else {
        Vec::new()
    };
    Ok(serde_json::json!({
        "messageId": row.id,
        "editCount": row.edit_count,
        "revisions": revisions.iter().map(|r| serde_json::json!({
            "revision": r.revision,
            "text": r.text,
            "createdAt": crate::conversations::service::rfc3339(r.created_at),
            "supersededAt": crate::conversations::service::rfc3339(r.superseded_at),
        })).collect::<Vec<_>>(),
    }))
}

/// Imports one legacy chat message (Part V H §101) inside the caller's transaction:
/// provenance `LEGACY_MANAGED`, the ORIGINAL time kept, idempotent per (conversation,
/// legacy source, legacy id) — `Ok(None)` for an already imported message. A legacy
/// message deleted at its source is imported as a tombstone without content.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn import_legacy<C: ConnectionTrait>(
    db: &C,
    ring: &ManagedKeyring,
    pool: &sea_orm::DatabaseConnection,
    ctx: &TrustedContext,
    conversation: &ConversationRow,
    sender: Uuid,
    legacy_source: &str,
    legacy_message_id: &str,
    text: Option<&str>,
    presentation: Option<&str>,
    original_created_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<Option<MessageRow>> {
    let exists = db::scalar_i64(
        db,
        "SELECT count(*) FROM messaging_messages WHERE conversation_id = $1 AND legacy_source = $2 AND legacy_message_id = $3",
        vec![conversation.id.into(), legacy_source.into(), legacy_message_id.into()],
    )
    .await?;
    if exists > 0 {
        return Ok(None);
    }
    let content = match text {
        Some(t) => Some(admit(t.to_string())?),
        None => None,
    };
    let sequence = allocate_sequence(db, conversation.id).await?;
    let id = Uuid::new_v4();
    let sealed = match &content {
        Some(c) => Some(
            ring.seal(
                pool,
                &SealScope {
                    tenant_id: ctx.scope.tenant_id,
                    application_id: ctx.scope.application_id,
                    environment_id: ctx.scope.environment_id,
                    conversation_id: conversation.id,
                    message_id: id,
                },
                c.text(),
            )
            .await?,
        ),
        None => None,
    };
    db::exec(
        db,
        "INSERT INTO messaging_messages (id, conversation_id, tenant_id, application_id, environment_id, sequence, \
           message_kind, security_profile, sender_principal_id, client_message_id, managed_sealed, managed_seal_nonce, \
           managed_seal_key_id, managed_search, attachment_count, created_at, managed_presentation, security_provenance, \
           legacy_source, legacy_message_id, original_created_at, lifecycle, lifecycle_reason, lifecycle_changed_at) \
         VALUES ($1, $2, $3, $4, $5, $6, 'APPLICATION', 'MANAGED', $7, $8, $9, $10, $11, \
           CASE WHEN $12::text IS NULL THEN NULL ELSE to_tsvector('simple', $12) END, 0, $13, $14, 'LEGACY_MANAGED', $15, $16, $17, \
           CASE WHEN $9::bytea IS NULL THEN 'DELETED' ELSE 'ACTIVE' END, \
           CASE WHEN $9::bytea IS NULL THEN 'LEGACY_DELETED' ELSE NULL END, \
           CASE WHEN $9::bytea IS NULL THEN $13 ELSE NULL END)",
        vec![
            id.into(),
            conversation.id.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            sequence.into(),
            sender.into(),
            Uuid::new_v4().into(),
            sealed.as_ref().map(|s| s.sealed.clone()).into(),
            sealed.as_ref().map(|s| s.nonce.clone()).into(),
            sealed.as_ref().map(|s| s.key_id.clone()).into(),
            content.as_ref().map(|c| c.text().to_string()).into(),
            now.into(),
            presentation.map(str::to_string).into(),
            legacy_source.into(),
            legacy_message_id.into(),
            original_created_at.into(),
        ],
    )
    .await?;
    Ok(Some(
        load_message(db, conversation.id, id, false)
            .await?
            .ok_or(Error::code(ErrorCode::InternalError))?,
    ))
}

/// The managed key ring, or `FEATURE_DISABLED` (MANAGED storage not configured).
pub(crate) fn require_ring(state: &AppState) -> Result<&ManagedKeyring> {
    ring(state)
}
