//! The message store and the durable conversation event log (Part V Workstream F, the
//! foundation of Parts E–G).
//!
//! * Every accepted message gets the conversation's next canonical SEQUENCE and commits
//!   in ONE transaction with its durable event (outbox row carrying the conversation's
//!   next EVENT cursor). No WebSocket, broker or Redis write precedes the commit.
//! * PRIVATE_E2EE ([`e2ee`]): opaque MLS ciphertext only. The server checks the PUBLIC
//!   MLS framing (group id, epoch, content type) like an MLS Delivery Service, binds its
//!   sequence / id to the ciphertext digest in the acceptance record, and never decrypts.
//! * MANAGED ([`managed`]): server-processable text, sealed at rest under the deployment's
//!   managed key ring (XChaCha20-Poly1305) and indexed for authorized search. Never
//!   reachable from the E2EE path.
//! * Deletion is a lifecycle (ACTIVE → DELETED / MODERATED / EXPIRED → PURGED). A
//!   tombstone hides content from every read, replay and fanout at once; the irreversible
//!   purge ([`crate::compliance::retention`]) removes the ciphertext / sealed text unless
//!   a legal hold protects it.
//! * Reads, replay and hydration honour each member's history window and the
//!   "delete for me" markers.

pub mod e2ee;
pub mod http;
pub mod managed;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::{
    conversations::{
        access::{self, Membership},
        policy,
        service::{request_context, rfc3339, scope_of, ConversationRow},
    },
    db,
    domain::{RequestContext, SecurityProfile},
    error::{Error, ErrorCode, Result},
    events::outbox::{self, NewEvent},
    http::middleware::Acting,
    identity::TrustedContext,
    limits::{self, Window},
    state::AppState,
};

pub(crate) fn decode_b64(field: &str, value: &str, max: usize) -> Result<Vec<u8>> {
    if value.len() > max.div_ceil(3) * 4 + 4 {
        return Err(Error::reason(ErrorCode::ContentRejected, "TOO_LARGE"));
    }
    URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| Error::validation(format!("{field} must be unpadded base64url")))
}

pub(crate) fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

// ---------------------------------------------------------------------------------------
// Rows and views
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, FromQueryResult)]
pub struct MessageRow {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub sequence: i64,
    pub message_kind: String,
    pub security_profile: String,
    pub sender_principal_id: Uuid,
    pub sender_device_id: Option<Uuid>,
    pub client_message_id: Uuid,
    pub target_message_id: Option<Uuid>,
    pub mls_epoch: Option<i64>,
    pub mls_ciphertext: Option<Vec<u8>>,
    pub ciphertext_sha256: Option<String>,
    pub acceptance_digest: Option<String>,
    pub managed_sealed: Option<Vec<u8>>,
    pub managed_seal_nonce: Option<Vec<u8>>,
    pub managed_seal_key_id: Option<String>,
    pub attachment_count: i32,
    pub lifecycle: String,
    pub lifecycle_reason: Option<String>,
    pub edit_count: i32,
    pub edited_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    /// `NATIVE` or `LEGACY_MANAGED` (imported legacy chat; MANAGED only, never E2EE).
    pub security_provenance: String,
    pub original_created_at: Option<DateTime<Utc>>,
    /// MANAGED only: `STANDARD` / `ANNOUNCEMENT` (`None`: standard).
    pub managed_presentation: Option<String>,
    /// `IN_APP` or `SMS` (arrived through the SMS Bridge; MANAGED only).
    pub channel: String,
    pub reply_to_message_id: Option<Uuid>,
    pub thread_root_id: Option<Uuid>,
}

pub const MESSAGE_COLUMNS: &str = "m.id, m.conversation_id, m.sequence, m.message_kind, m.security_profile, \
     m.sender_principal_id, m.sender_device_id, m.client_message_id, m.target_message_id, m.mls_epoch, \
     m.mls_ciphertext, m.ciphertext_sha256, m.acceptance_digest, m.managed_sealed, m.managed_seal_nonce, \
     m.managed_seal_key_id, m.attachment_count, m.lifecycle, m.lifecycle_reason, m.edit_count, m.edited_at, \
     m.created_at, m.tenant_id, m.application_id, m.environment_id, m.security_provenance, m.original_created_at, \
     m.managed_presentation, m.channel, m.reply_to_message_id, m.thread_root_id";

impl MessageRow {
    pub fn profile(&self) -> SecurityProfile {
        SecurityProfile::parse(&self.security_profile).unwrap_or(SecurityProfile::PrivateE2ee)
    }

    pub fn is_active(&self) -> bool {
        self.lifecycle == "ACTIVE"
    }
}

/// The client representation of a message. Content (ciphertext or managed text) only
/// while ACTIVE; tombstones carry their state and nothing that was content.
pub async fn render(state: &AppState, row: &MessageRow, hidden: bool) -> Value {
    let attachments = if row.attachment_count > 0 && row.is_active() {
        crate::attachments::message_attachment_ids(&state.db, row.id)
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let show = row.is_active() && !hidden;
    let mut v = json!({
        "messageId": row.id,
        "conversationId": row.conversation_id,
        "sequence": row.sequence,
        "kind": row.message_kind,
        "securityProfile": row.security_profile,
        "senderPrincipalId": row.sender_principal_id,
        "senderDeviceId": row.sender_device_id,
        "clientMessageId": row.client_message_id,
        "targetMessageId": row.target_message_id,
        "lifecycle": if hidden && row.is_active() { "HIDDEN_FOR_YOU" } else { row.lifecycle.as_str() },
        "lifecycleReason": row.lifecycle_reason,
        "editCount": row.edit_count,
        "editedAt": row.edited_at.map(rfc3339),
        "createdAt": rfc3339(row.created_at),
        "attachmentIds": if show { json!(attachments) } else { json!([]) },
        "securityProvenance": row.security_provenance,
        "originalCreatedAt": row.original_created_at.map(rfc3339),
        "channel": row.channel,
        "replyToMessageId": row.reply_to_message_id,
        "threadRootId": row.thread_root_id,
    });
    match row.profile() {
        SecurityProfile::PrivateE2ee => {
            v["e2ee"] = json!({
                "epoch": row.mls_epoch,
                "ciphertext": if show { row.mls_ciphertext.as_deref().map(b64) } else { None },
                "ciphertextSha256": row.ciphertext_sha256,
                "acceptanceDigest": row.acceptance_digest,
            });
        }
        SecurityProfile::Managed => {
            let text = if show {
                managed::text_of(state, row)
            } else {
                None
            };
            v["managed"] = json!({
                "text": text,
                "presentation": row.managed_presentation.as_deref().unwrap_or("STANDARD"),
            });
        }
    }
    v
}

pub async fn load_message<C: ConnectionTrait>(
    db: &C,
    conversation_id: Uuid,
    message_id: Uuid,
    lock: bool,
) -> Result<Option<MessageRow>> {
    Ok(db::one(
        db,
        &format!(
            "SELECT {MESSAGE_COLUMNS} FROM messaging_messages m WHERE m.conversation_id = $1 AND m.id = $2{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        vec![conversation_id.into(), message_id.into()],
    )
    .await?)
}

pub async fn load_by_client_id<C: ConnectionTrait>(
    db: &C,
    conversation_id: Uuid,
    sender: Uuid,
    client_message_id: Uuid,
) -> Result<Option<MessageRow>> {
    Ok(db::one(
        db,
        &format!(
            "SELECT {MESSAGE_COLUMNS} FROM messaging_messages m WHERE m.conversation_id = $1 \
               AND m.sender_principal_id = $2 AND m.client_message_id = $3"
        ),
        vec![
            conversation_id.into(),
            sender.into(),
            client_message_id.into(),
        ],
    )
    .await?)
}

pub(crate) async fn hidden_for<C: ConnectionTrait>(
    db: &C,
    principal: Uuid,
    ids: &[Uuid],
) -> Result<Vec<Uuid>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    #[derive(FromQueryResult)]
    struct H {
        message_id: Uuid,
    }
    let rows: Vec<H> = db::all(
        db,
        "SELECT message_id FROM messaging_message_hidden WHERE principal_id = $1 AND message_id = ANY($2)",
        vec![principal.into(), ids.to_vec().into()],
    )
    .await?;
    Ok(rows.into_iter().map(|h| h.message_id).collect())
}

// ---------------------------------------------------------------------------------------
// Sending
// ---------------------------------------------------------------------------------------

/// `POST /v1/conversations/{id}/messages`. Which fields apply is decided by the
/// conversation's PERSISTED profile, never by the request.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SendRequest {
    pub client_message_id: Uuid,
    /// PRIVATE_E2EE: the sending device, the MLS epoch and the `MLSMessage` bytes.
    #[serde(default)]
    pub device_id: Option<Uuid>,
    #[serde(default)]
    pub epoch: Option<u64>,
    #[serde(default)]
    pub ciphertext: Option<String>,
    /// MANAGED: server-processable text.
    #[serde(default)]
    pub text: Option<String>,
    /// `APPLICATION` (default) or `EDIT` (PRIVATE_E2EE edits are new ciphertexts).
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub target_message_id: Option<Uuid>,
    #[serde(default)]
    pub attachment_ids: Vec<Uuid>,
    /// `false`: no push notification jobs for this message (mirrored / migrated writes).
    #[serde(default)]
    pub notify: Option<bool>,
    /// MANAGED only: `STANDARD` (default) or `ANNOUNCEMENT`.
    #[serde(default)]
    pub managed_presentation: Option<String>,
    /// A reply to this earlier message of the conversation (thread metadata, ids only).
    #[serde(default)]
    pub reply_to_message_id: Option<Uuid>,
}

/// Checks shared by both profiles, before the transaction: membership, lifecycle,
/// sending policy, mute, abuse limits.
pub(crate) async fn send_prelude(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
) -> Result<(ConversationRow, Membership, policy::EffectivePolicy)> {
    let now = state.now();
    let (conversation, member) =
        access::as_member(&state.db, ctx, conversation_id, acting.principal_id).await?;
    access::require_accepting(&conversation, now)?;
    let effective = policy::current(&state.db, ctx.scope.environment_id).await?;
    if !effective.sending_enabled {
        return Err(Error::reason(
            ErrorCode::FeatureDisabled,
            "SENDING_DISABLED",
        ));
    }
    if !member.may_send(now) {
        return Err(Error::reason(ErrorCode::SendRestricted, "MUTED"));
    }
    crate::safety::abuse::require_not_restricted(state, ctx, acting.principal_id).await?;
    // Tenant fairness first (one environment cannot monopolize shared capacity), then
    // the principal's own velocity.
    let env_key = format!("msg-env:{}", ctx.scope.environment_id.simple());
    if let Some(retry) = limits::hit(
        &state.db,
        &env_key,
        Window::Minute,
        state.conversations.environment_messages_per_minute,
        now,
    )
    .await?
    {
        return Err(Error::RateLimited {
            retry_after_seconds: retry,
        });
    }
    let key = format!(
        "msg:{}:{}",
        ctx.scope.environment_id.simple(),
        acting.principal_id.simple()
    );
    if let Some(retry) = limits::hit(
        &state.db,
        &key,
        Window::Minute,
        effective.abuse.messages_per_minute,
        now,
    )
    .await?
    {
        crate::safety::abuse::signal(state, ctx, acting.principal_id, "MESSAGE_VELOCITY").await;
        return Err(Error::RateLimited {
            retry_after_seconds: retry,
        });
    }
    if conversation.context_class == "DIRECT"
        && crate::safety::blocks::direct_blocked(&state.db, &conversation, acting.principal_id)
            .await?
    {
        return Err(Error::code(ErrorCode::DeliveryRefused));
    }
    Ok((conversation, member, effective))
}

/// Re-checks, INSIDE the transaction, everything a concurrent change could have revoked
/// since the prelude: the member row (shared lock: a removal / mute waits for us or we
/// see it), the conversation row (shared lock against lock / close) and, for DIRECT
/// conversations, blocks (the pair's advisory lock serializes with block creation).
pub(crate) async fn lock_sender<C: ConnectionTrait>(
    db: &C,
    conversation: &ConversationRow,
    principal: Uuid,
    now: DateTime<Utc>,
) -> Result<()> {
    #[derive(FromQueryResult)]
    struct L {
        lifecycle_state: String,
    }
    let c: L = db::one(
        db,
        "SELECT lifecycle_state FROM messaging_conversations WHERE id = $1 FOR SHARE",
        vec![conversation.id.into()],
    )
    .await?
    .ok_or(Error::code(ErrorCode::NotFound))?;
    if c.lifecycle_state != "ACTIVE" {
        return Err(Error::reason(ErrorCode::ConversationNotActive, "CHANGED"));
    }
    let m: Option<Membership> = db::one(
        db,
        "SELECT principal_id, role, state, send_restricted_until, visible_from_sequence, visible_from_event, version \
         FROM messaging_conversation_members WHERE conversation_id = $1 AND principal_id = $2 FOR SHARE",
        vec![conversation.id.into(), principal.into()],
    )
    .await?;
    match m {
        Some(m) if m.may_send(now) => {}
        Some(m) if m.is_active() => return Err(Error::reason(ErrorCode::SendRestricted, "MUTED")),
        _ => return Err(Error::code(ErrorCode::NotFound)),
    }
    if conversation.context_class == "DIRECT" {
        crate::safety::blocks::lock_pair_shared(db, conversation).await?;
        if crate::safety::blocks::direct_blocked(db, conversation, principal).await? {
            return Err(Error::code(ErrorCode::DeliveryRefused));
        }
    }
    Ok(())
}

pub async fn send(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    request: SendRequest,
) -> Result<(u16, Value)> {
    if request.client_message_id.is_nil() {
        return Err(Error::validation("clientMessageId must not be nil"));
    }
    let (conversation, _member, effective) =
        send_prelude(state, ctx, acting, conversation_id).await?;
    if !request.attachment_ids.is_empty() {
        crate::attachments::check_message_attachments(
            state,
            &conversation,
            &effective,
            acting.principal_id,
            &request.attachment_ids,
        )
        .await?;
    }
    match conversation.profile() {
        SecurityProfile::PrivateE2ee => {
            e2ee::send(state, ctx, acting, &conversation, &effective, request).await
        }
        SecurityProfile::Managed => {
            managed::send(state, ctx, acting, &conversation, &effective, request).await
        }
    }
}

/// Records the durable event of a newly accepted message (same transaction).
pub(crate) async fn record_created<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    row: &MessageRow,
    request: &RequestContext,
    now: DateTime<Utc>,
) -> Result<i64> {
    record_created_notify(db, ctx, row, request, now, true).await
}

/// [`record_created`] with control over push notification jobs (`notify: false` for
/// mirrored, migrated and imported messages).
pub(crate) async fn record_created_notify<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    row: &MessageRow,
    request: &RequestContext,
    now: DateTime<Utc>,
    notify: bool,
) -> Result<i64> {
    let (event_type, payload) = match row.message_kind.as_str() {
        "EDIT" => (
            "messaging.message.edited",
            json!({
                "message_id": row.id,
                "target_message_id": row.target_message_id,
                "sequence": row.sequence,
                "sender_principal_id": row.sender_principal_id,
            }),
        ),
        _ => (
            "messaging.message.created",
            json!({
                "message_id": row.id,
                "sequence": row.sequence,
                "message_kind": row.message_kind,
                "sender_principal_id": row.sender_principal_id,
                "sender_device_id": row.sender_device_id,
                "security_provenance": row.security_provenance,
                "notify": notify,
            }),
        ),
    };
    let (_, cursor) = outbox::enqueue_conversation(
        db,
        row.conversation_id,
        NewEvent {
            event_type,
            aggregate_type: "CONVERSATION",
            aggregate_id: row.conversation_id,
            scope: scope_of(ctx),
            payload,
        },
        request,
        now,
    )
    .await?;
    if row.message_kind == "APPLICATION" {
        crate::usage::record(
            db,
            ctx.into(),
            crate::usage::MESSAGE_ACCEPTED,
            1,
            row.id,
            Some(&row.security_profile),
            &format!("message:{}", row.id),
            now,
        )
        .await?;
    }
    crate::metrics::inc(
        "messaging_messages_accepted_total",
        &[(
            "profile",
            if row.security_profile == "MANAGED" {
                "managed"
            } else {
                "e2ee"
            },
        )],
    );
    Ok(cursor)
}

// ---------------------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListQuery {
    #[serde(default)]
    pub after_sequence: Option<i64>,
    /// Newest first, strictly before this sequence (history pagination); exclusive with
    /// `afterSequence`.
    #[serde(default)]
    pub before_sequence: Option<i64>,
    /// Only this thread: the root message and its replies.
    #[serde(default)]
    pub thread_root_id: Option<Uuid>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// Messages after a sequence, in canonical order, within the member's history window.
pub async fn list(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    q: ListQuery,
) -> Result<Value> {
    let limit = q.limit.unwrap_or(100);
    if !(1..=500).contains(&limit) {
        return Err(Error::validation("limit must be between 1 and 500"));
    }
    let (_, member) =
        access::as_member(&state.db, ctx, conversation_id, acting.principal_id).await?;
    if q.after_sequence.is_some() && q.before_sequence.is_some() {
        return Err(Error::validation(
            "afterSequence and beforeSequence are exclusive",
        ));
    }
    let backwards = q.before_sequence.is_some();
    let rows: Vec<MessageRow> = if let Some(before) = q.before_sequence {
        db::all(
            &state.db,
            &format!(
                "SELECT {MESSAGE_COLUMNS} FROM messaging_messages m WHERE m.conversation_id = $1 AND m.sequence < $2 \
                   AND m.sequence >= $3 AND ($5::uuid IS NULL OR m.thread_root_id = $5 OR m.id = $5) \
                 ORDER BY m.sequence DESC LIMIT $4"
            ),
            vec![conversation_id.into(), before.into(), member.visible_from_sequence.into(), (limit + 1).into(), q.thread_root_id.into()],
        )
        .await?
    } else {
        let after = q
            .after_sequence
            .unwrap_or(0)
            .max(member.visible_from_sequence - 1);
        db::all(
            &state.db,
            &format!(
                "SELECT {MESSAGE_COLUMNS} FROM messaging_messages m WHERE m.conversation_id = $1 AND m.sequence > $2 \
                   AND ($4::uuid IS NULL OR m.thread_root_id = $4 OR m.id = $4) \
                 ORDER BY m.sequence LIMIT $3"
            ),
            vec![conversation_id.into(), after.into(), (limit + 1).into(), q.thread_root_id.into()],
        )
        .await?
    };
    let more = rows.len() as i64 > limit;
    let page: Vec<_> = rows.into_iter().take(limit as usize).collect();
    let hidden = hidden_for(
        &state.db,
        acting.principal_id,
        &page.iter().map(|r| r.id).collect::<Vec<_>>(),
    )
    .await?;
    let mut items = Vec::with_capacity(page.len());
    for r in &page {
        items.push(render(state, r, hidden.contains(&r.id)).await);
    }
    Ok(json!({
        "messages": items,
        "nextAfterSequence": if more && !backwards { page.last().map(|r| r.sequence) } else { None },
        "nextBeforeSequence": if more && backwards { page.last().map(|r| r.sequence) } else { None },
    }))
}

/// Permalink resolution (§84): a reference is never authority; every resolution checks
/// current membership. A non-member, an unknown id and a message before the member's
/// history window are the same `NOT_FOUND`; members see tombstones by state.
pub async fn resolve(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    message_id: Uuid,
) -> Result<Value> {
    let (_, member) =
        access::as_member(&state.db, ctx, conversation_id, acting.principal_id).await?;
    let row = load_message(&state.db, conversation_id, message_id, false)
        .await?
        .filter(|r| r.sequence >= member.visible_from_sequence)
        .ok_or(Error::code(ErrorCode::NotFound))?;
    let hidden = !hidden_for(&state.db, acting.principal_id, &[row.id])
        .await?
        .is_empty();
    Ok(json!({
        "resolution": if hidden && row.is_active() { "HIDDEN_FOR_YOU" } else { row.lifecycle.as_str() },
        "message": render(state, &row, hidden).await,
    }))
}

// ---------------------------------------------------------------------------------------
// Deletion
// ---------------------------------------------------------------------------------------

/// Applies a server-side tombstone (DELETED / MODERATED / EXPIRED) to an ACTIVE message
/// inside the caller's transaction and records its durable event. The content stays
/// stored until the purge worker removes it (unless a legal hold protects it).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn tombstone<C: ConnectionTrait>(
    db: &C,
    ctx_scope: &crate::events::outbox::EventScope,
    row: &MessageRow,
    lifecycle: &'static str,
    reason: &str,
    purge_after: DateTime<Utc>,
    request: &RequestContext,
    now: DateTime<Utc>,
) -> Result<()> {
    let updated = db::exec(
        db,
        "UPDATE messaging_messages SET lifecycle = $2, lifecycle_reason = $3, lifecycle_changed_at = $4, \
           purge_after = $5, managed_search = NULL WHERE id = $1 AND lifecycle = 'ACTIVE'",
        vec![row.id.into(), lifecycle.into(), reason.into(), now.into(), purge_after.into()],
    )
    .await?;
    if updated != 1 {
        return Err(Error::reason(ErrorCode::MessageNotModifiable, "NOT_ACTIVE"));
    }
    // Attachments of a tombstoned message stop being downloadable at once.
    crate::attachments::follow_message(db, row.id, lifecycle, now).await?;
    let event_type = match lifecycle {
        "MODERATED" => "messaging.message.moderated",
        "EXPIRED" => "messaging.message.expired",
        _ => "messaging.message.deleted",
    };
    outbox::enqueue_conversation(
        db,
        row.conversation_id,
        NewEvent {
            event_type,
            aggregate_type: "CONVERSATION",
            aggregate_id: row.conversation_id,
            scope: *ctx_scope,
            payload: json!({
                "message_id": row.id,
                "sequence": row.sequence,
                "lifecycle": lifecycle,
                "reason": reason,
            }),
        },
        request,
        now,
    )
    .await?;
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteQuery {
    /// `EVERYONE` (default; sender only): a server tombstone for every member.
    /// `ME`: hidden on the acting principal's devices only.
    #[serde(default)]
    pub scope: Option<String>,
}

pub async fn delete(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    message_id: Uuid,
    q: DeleteQuery,
) -> Result<Value> {
    let (_, member) =
        access::as_member(&state.db, ctx, conversation_id, acting.principal_id).await?;
    let effective = policy::current(&state.db, ctx.scope.environment_id).await?;
    match q.scope.as_deref().unwrap_or("EVERYONE") {
        "ME" => {
            let row = load_message(&state.db, conversation_id, message_id, false)
                .await?
                .filter(|r| r.sequence >= member.visible_from_sequence)
                .ok_or(Error::code(ErrorCode::NotFound))?;
            db::exec(
                &state.db,
                "INSERT INTO messaging_message_hidden (message_id, principal_id, hidden_at) VALUES ($1, $2, $3) \
                 ON CONFLICT DO NOTHING",
                vec![row.id.into(), acting.principal_id.into(), state.now().into()],
            )
            .await?;
            Ok(json!({ "messageId": row.id, "deletion": "HIDDEN_FOR_YOU" }))
        }
        "EVERYONE" => {
            let st = state.clone();
            let ctx2 = ctx.clone();
            let acting = acting.clone();
            let request_ctx = request_context();
            state
                .db
                .transaction::<_, Value, Error>(move |txn| {
                    Box::pin(async move {
                        let now = st.now();
                        let row = load_message(txn, conversation_id, message_id, true)
                            .await?
                            .filter(|r| r.sequence >= member.visible_from_sequence)
                            .ok_or(Error::code(ErrorCode::NotFound))?;
                        if row.sender_principal_id != acting.principal_id {
                            return Err(Error::reason(ErrorCode::PrincipalNotAuthorized, "NOT_SENDER"));
                        }
                        if row.message_kind == "MLS_COMMIT" {
                            return Err(Error::reason(ErrorCode::MessageNotModifiable, "HANDSHAKE"));
                        }
                        if !row.is_active() {
                            return Ok(json!({ "messageId": row.id, "deletion": row.lifecycle }));
                        }
                        if let Some(w) = effective.editing.delete_window_seconds {
                            if now > row.created_at + chrono::Duration::seconds(w) {
                                return Err(Error::reason(ErrorCode::MessageNotModifiable, "WINDOW_CLOSED"));
                            }
                        }
                        let grace = chrono::Duration::seconds(effective.retention.deleted_purge_grace_seconds);
                        tombstone(txn, &scope_of(&ctx2), &row, "DELETED", "DELETED_BY_SENDER", now + grace, &request_ctx, now)
                            .await?;
                        Ok(json!({ "messageId": row.id, "deletion": "DELETED", "purgeAfter": rfc3339(now + grace) }))
                    })
                })
                .await
                .map_err(Error::from)
        }
        _ => Err(Error::validation("scope must be EVERYONE or ME")),
    }
}

// ---------------------------------------------------------------------------------------
// Read positions
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadCursorRequest {
    pub sequence: i64,
}

/// Read receipts are distinct from transport delivery and application
/// acknowledgements: a monotonic read position per principal, shared with the
/// conversation (as a durable event) only when the privacy policy enables receipts.
pub async fn set_read_cursor(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    request: ReadCursorRequest,
) -> Result<Value> {
    let (conversation, _) =
        access::as_member(&state.db, ctx, conversation_id, acting.principal_id).await?;
    let effective = policy::current(&state.db, ctx.scope.environment_id).await?;
    let st = state.clone();
    let ctx = ctx.clone();
    let acting = acting.clone();
    let request_ctx = request_context();
    state
        .db
        .transaction::<_, Value, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let max = db::scalar_i64(
                    txn,
                    "SELECT next_sequence - 1 FROM messaging_conversation_sequences WHERE conversation_id = $1",
                    vec![conversation.id.into()],
                )
                .await?;
                if request.sequence < 0 || request.sequence > max {
                    return Err(Error::validation("sequence is beyond the conversation's last message"));
                }
                let current = db::scalar_i64(
                    txn,
                    "INSERT INTO messaging_read_cursors (conversation_id, principal_id, read_sequence, updated_at) \
                     VALUES ($1, $2, $3, $4) ON CONFLICT (conversation_id, principal_id) DO UPDATE \
                       SET read_sequence = GREATEST(messaging_read_cursors.read_sequence, EXCLUDED.read_sequence), \
                           updated_at = EXCLUDED.updated_at \
                     RETURNING read_sequence",
                    vec![conversation.id.into(), acting.principal_id.into(), request.sequence.into(), now.into()],
                )
                .await?;
                // Synchronizing past a message delivers it (§52).
                advance_delivery(
                    txn,
                    scope_of(&ctx),
                    conversation.id,
                    acting.principal_id,
                    current,
                    effective.privacy.delivery_receipts,
                    now,
                )
                .await?;
                // §55: the principal's other sessions follow its read position, whether or
                // not read receipts are shared with other members.
                if current == request.sequence {
                    outbox::record(
                        txn,
                        "messaging.read-cursor.synced",
                        "CONVERSATION",
                        conversation.id,
                        scope_of(&ctx),
                        json!({ "conversation_id": conversation.id, "principal_id": acting.principal_id, "read_sequence": current }),
                        now,
                    )
                    .await?;
                }
                if effective.privacy.read_receipts && current == request.sequence {
                    outbox::enqueue_conversation(
                        txn,
                        conversation.id,
                        NewEvent {
                            event_type: "messaging.read-cursor.updated",
                            aggregate_type: "CONVERSATION",
                            aggregate_id: conversation.id,
                            scope: scope_of(&ctx),
                            payload: json!({ "principal_id": acting.principal_id, "read_sequence": current }),
                        },
                        &request_ctx,
                        now,
                    )
                    .await?;
                }
                Ok(json!({ "conversationId": conversation.id, "readSequence": current }))
            })
        })
        .await
        .map_err(Error::from)
}

/// Advances a member's delivery cursor (§52: DELIVERED once a session acknowledged the
/// message or the member synchronized past it) and, when the policy shares delivery
/// receipts, records `messaging.delivery-cursor.updated` in the same transaction.
/// Returns the new delivered sequence when it moved.
pub(crate) async fn advance_delivery<C: ConnectionTrait>(
    txn: &C,
    scope: crate::events::outbox::EventScope,
    conversation_id: Uuid,
    principal_id: Uuid,
    sequence: i64,
    share: bool,
    now: DateTime<Utc>,
) -> Result<Option<i64>> {
    if sequence <= 0 {
        return Ok(None);
    }
    let moved: Option<i64> = db::one::<DeliveredRow, _>(
        txn,
        "INSERT INTO messaging_delivery_cursors (conversation_id, principal_id, delivered_sequence, updated_at) \
         VALUES ($1, $2, $3, $4) ON CONFLICT (conversation_id, principal_id) DO UPDATE \
           SET delivered_sequence = EXCLUDED.delivered_sequence, updated_at = EXCLUDED.updated_at \
           WHERE messaging_delivery_cursors.delivered_sequence < EXCLUDED.delivered_sequence \
         RETURNING delivered_sequence",
        vec![conversation_id.into(), principal_id.into(), sequence.into(), now.into()],
    )
    .await?
    .map(|r| r.delivered_sequence);
    if let (Some(delivered), true) = (moved, share) {
        outbox::enqueue_conversation(
            txn,
            conversation_id,
            NewEvent {
                event_type: "messaging.delivery-cursor.updated",
                aggregate_type: "CONVERSATION",
                aggregate_id: conversation_id,
                scope,
                payload: json!({ "principal_id": principal_id, "delivered_sequence": delivered }),
            },
            &request_context(),
            now,
        )
        .await?;
    }
    Ok(moved)
}

#[derive(Debug, FromQueryResult)]
struct DeliveredRow {
    delivered_sequence: i64,
}

/// The highest APPLICATION message sequence among the `message.created` events at or
/// before an acknowledged conversation event position (MLS handshake messages are not
/// "delivered" to a person).
pub(crate) async fn message_sequence_at<C: ConnectionTrait>(
    db: &C,
    conversation_id: Uuid,
    event_sequence: i64,
) -> Result<i64> {
    Ok(db::scalar_i64(
        db,
        "SELECT COALESCE((SELECT (payload->>'sequence')::bigint FROM messaging_outbox_events \
            WHERE conversation_id = $1 AND conversation_event_sequence <= $2 AND event_type = 'messaging.message.created' \
              AND payload->>'message_kind' = 'APPLICATION' \
            ORDER BY conversation_event_sequence DESC LIMIT 1), 0)",
        vec![conversation_id.into(), event_sequence.into()],
    )
    .await?)
}

// ---------------------------------------------------------------------------------------
// The durable conversation event log (replay)
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, FromQueryResult)]
pub struct EventRow {
    pub id: Uuid,
    pub event_type: String,
    pub schema_version: i16,
    pub conversation_id: Option<Uuid>,
    pub conversation_event_sequence: Option<i64>,
    pub payload: Value,
    pub occurred_at: DateTime<Utc>,
}

/// Durable events of `conversation_id` after `after` (exclusive), oldest first.
pub async fn events_after<C: ConnectionTrait>(
    db: &C,
    conversation_id: Uuid,
    after: i64,
    limit: i64,
) -> Result<Vec<EventRow>> {
    Ok(db::all(
        db,
        "SELECT id, event_type, schema_version, conversation_id, conversation_event_sequence, payload, occurred_at \
         FROM messaging_outbox_events WHERE conversation_id = $1 AND conversation_event_sequence > $2 \
         ORDER BY conversation_event_sequence LIMIT $3",
        vec![conversation_id.into(), after.into(), limit.into()],
    )
    .await?)
}

/// The oldest replayable event of a conversation (the replay window's start).
pub async fn oldest_event<C: ConnectionTrait>(db: &C, conversation_id: Uuid) -> Result<i64> {
    db::scalar_i64(
        db,
        "SELECT COALESCE(min(conversation_event_sequence), 0) FROM messaging_outbox_events WHERE conversation_id = $1",
        vec![conversation_id.into()],
    )
    .await
    .map_err(Error::from)
}

fn camel(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    let mut up = false;
    for c in key.chars() {
        if c == '_' {
            up = true;
        } else if up {
            out.extend(c.to_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// The client event type of an outbox event type (`messaging.message.created` →
/// `message.created`).
pub fn client_type(event_type: &str) -> String {
    event_type
        .strip_prefix("messaging.")
        .unwrap_or(event_type)
        .replace("read-cursor", "read.cursor")
        .replace("delivery-cursor", "delivery.cursor")
        .replace("lifecycle-changed", "lifecycle.changed")
}

/// Hydrates durable events for one viewing member: message events carry the message's
/// CURRENT representation (a later deletion means the replay shows a tombstone, never
/// the old content); every other event carries its payload.
pub async fn hydrate(
    state: &AppState,
    viewer: Uuid,
    member: &Membership,
    events: &[EventRow],
) -> Result<Vec<Value>> {
    let ids: Vec<Uuid> = events
        .iter()
        .filter_map(|e| e.payload.get("message_id").and_then(Value::as_str))
        .filter_map(|s| Uuid::parse_str(s).ok())
        .collect();
    let rows: Vec<MessageRow> = if ids.is_empty() {
        Vec::new()
    } else {
        db::all(
            &state.db,
            &format!("SELECT {MESSAGE_COLUMNS} FROM messaging_messages m WHERE m.id = ANY($1)"),
            vec![ids.clone().into()],
        )
        .await?
    };
    let hidden = hidden_for(&state.db, viewer, &ids).await?;
    let mut out = Vec::with_capacity(events.len());
    for e in events {
        let mut data = Map::new();
        if let Some(obj) = e.payload.as_object() {
            for (k, v) in obj {
                if k != "message_id" {
                    data.insert(camel(k), v.clone());
                }
            }
        }
        if let Some(mid) = e
            .payload
            .get("message_id")
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
        {
            match rows.iter().find(|r| r.id == mid) {
                Some(r) if r.sequence >= member.visible_from_sequence => {
                    data.insert(
                        "message".into(),
                        render(state, r, hidden.contains(&r.id)).await,
                    );
                }
                _ => {
                    data.insert("messageId".into(), json!(mid));
                }
            }
        }
        out.push(json!({
            "eventId": format!("evt_{}", e.id.simple()),
            "type": client_type(&e.event_type),
            "schemaVersion": e.schema_version,
            "conversationId": e.conversation_id,
            "eventSequence": e.conversation_event_sequence,
            "occurredAt": rfc3339(e.occurred_at),
            "data": Value::Object(data),
        }));
    }
    Ok(out)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventsQuery {
    #[serde(default)]
    pub after: Option<i64>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// `GET /v1/conversations/{id}/events`: the REST replay / synchronization path (also
/// what a realtime client is sent to when its replay window is gone).
pub async fn replay(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    q: EventsQuery,
) -> Result<Value> {
    let limit = q.limit.unwrap_or(200);
    if !(1..=1000).contains(&limit) {
        return Err(Error::validation("limit must be between 1 and 1000"));
    }
    let (_, member) =
        access::as_member(&state.db, ctx, conversation_id, acting.principal_id).await?;
    let requested = q.after.unwrap_or(member.visible_from_event);
    if requested < 0 {
        return Err(Error::validation("after must be >= 0"));
    }
    let after = requested.max(member.visible_from_event);
    let oldest = oldest_event(&state.db, conversation_id).await?;
    // Events before the retained window were pruned: the client must resynchronize
    // messages by sequence instead of trusting a partial event history.
    let window_gone = oldest > 0 && after + 1 < oldest;
    let rows = events_after(&state.db, conversation_id, after, limit + 1).await?;
    let more = rows.len() as i64 > limit;
    let page: Vec<_> = rows.into_iter().take(limit as usize).collect();
    let events = hydrate(state, acting.principal_id, &member, &page).await?;
    Ok(json!({
        "events": events,
        "nextAfter": page.last().and_then(|e| e.conversation_event_sequence).or(Some(after)),
        "hasMore": more,
        "syncRequired": window_gone,
    }))
}
