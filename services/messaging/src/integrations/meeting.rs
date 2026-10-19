//! Meeting chat integration (Part V H §§97–101).
//!
//! The Meeting Service owns meeting sessions, admission, host permissions and
//! meeting-specific policy; Messaging owns the meeting CONVERSATION, its members, the
//! canonical messages, their lifecycle, realtime delivery, retention and audit.
//!
//! * [`resolve`] — an eligible conference session resolves its `MEETING` conversation
//!   idempotently by its opaque meeting reference (context namespace `meeting`): a retry
//!   resolves the same conversation, never a duplicate. The security profile is the one
//!   requested and allowed by policy; PRIVATE_E2EE that cannot be established fails
//!   (`E2EE_NOT_AVAILABLE`) — never a silent MANAGED fallback; a regulated MANAGED
//!   context needs the disclosure acknowledgement.
//! * [`sync_member`] / [`sync_lifecycle`] — admission, removal, ban, role and leave, and
//!   the meeting's end, applied in source-version order (fenced): duplicates and stale
//!   updates are ignored. Removal of a PRIVATE_E2EE member requires an MLS commit from a
//!   remaining member (`mlsRekeyRequired`) — the database removal is NOT cryptographic
//!   exclusion and is never reported as such.
//! * [`import_legacy`] — legacy meeting chat imported with `LEGACY_MANAGED` provenance
//!   into MANAGED conversations only, idempotently, with the original timestamp.
//!
//! Only conversations of the `meeting` context namespace in the caller's own delegated
//! environment are reachable here; any other conversation is `NOT_FOUND`.

use chrono::{DateTime, Utc};
use sea_orm::TransactionTrait;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::advance_fence;
use crate::{
    audit::{self, AuditEntry},
    conversations::{
        service::{self, request_context, ConversationRow, CreateRequest},
        ContextClass, ContextRef, LifecycleState,
    },
    db,
    domain::SecurityProfile,
    error::{Error, ErrorCode, Result},
    http::middleware::Acting,
    identity::TrustedContext,
    membership::{self, Administrator},
    messages::managed,
    state::AppState,
};

/// Context namespace of meeting conversations.
pub const NAMESPACE: &str = "meeting";
/// Fence source of meeting-authority updates.
pub const SOURCE: &str = "meeting-service";
pub const MAX_IMPORT_BATCH: usize = 200;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolveRequest {
    /// Opaque meeting / session reference (`[A-Za-z0-9._:/-]{1,256}`), never a secret.
    pub meeting_ref: String,
    /// The conversation owner (the meeting host's principal).
    pub owner_principal_id: Uuid,
    pub security_profile: SecurityProfile,
    #[serde(default)]
    pub managed_disclosure_acknowledged: bool,
}

/// `POST /internal/v1/meeting-conversations`.
pub async fn resolve(
    state: &AppState,
    ctx: &TrustedContext,
    owner: &Acting,
    request: ResolveRequest,
) -> Result<(u16, Value)> {
    let context = ContextRef {
        namespace: NAMESPACE.into(),
        reference: request.meeting_ref.clone(),
        version: 1,
    };
    context.validate().map_err(Error::validation)?;
    // A deterministic idempotency key per meeting reference: concurrent / retried
    // resolutions converge on one conversation.
    let key = format!(
        "meeting.{}",
        &hex::encode(Sha256::digest(request.meeting_ref.as_bytes()))[..40]
    );
    let created = service::create(
        state,
        ctx,
        owner,
        CreateRequest {
            context_class: ContextClass::Meeting,
            security_profile: Some(request.security_profile),
            counterpart_principal_id: None,
            context: Some(context),
            expires_at: None,
            expected_policy_version: None,
            managed_disclosure_acknowledged: request.managed_disclosure_acknowledged,
        },
        &key,
    )
    .await?;
    Ok((
        created.status,
        json!({ "created": created.status == 201, "conversation": created.body }),
    ))
}

/// Loads (and locks) a meeting conversation of the caller's environment.
async fn meeting_conversation<C: sea_orm::ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    id: Uuid,
) -> Result<ConversationRow> {
    service::load(db, ctx, id, true)
        .await?
        .filter(|c| c.context_namespace.as_deref() == Some(NAMESPACE))
        .ok_or(Error::code(ErrorCode::NotFound))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MemberSync {
    /// `ACTIVE` (admitted), `REMOVED`, `BANNED` or `LEFT`.
    pub state: String,
    /// `MEMBER` (default) or `ADMIN` for ACTIVE members.
    #[serde(default)]
    pub role: Option<String>,
    /// Monotonic version of the meeting's authority state (e.g. its event sequence).
    pub source_version: i64,
    #[serde(default)]
    pub reason: Option<String>,
}

/// `PUT /internal/v1/meeting-conversations/{id}/members/{principalId}`. `principal` is
/// already verified ACTIVE in the scope for `ACTIVE` updates (removals never require it:
/// an inactive principal must still lose access).
pub async fn sync_member(
    state: &AppState,
    ctx: &TrustedContext,
    conversation_id: Uuid,
    principal: Uuid,
    request: MemberSync,
) -> Result<Value> {
    if !matches!(
        request.state.as_str(),
        "ACTIVE" | "REMOVED" | "BANNED" | "LEFT"
    ) {
        return Err(Error::validation(
            "state must be ACTIVE, REMOVED, BANNED or LEFT",
        ));
    }
    let role = request.role.clone().unwrap_or_else(|| "MEMBER".into());
    if !matches!(role.as_str(), "MEMBER" | "ADMIN") {
        return Err(Error::validation("role must be MEMBER or ADMIN"));
    }
    if request.source_version < 0 {
        return Err(Error::validation("sourceVersion must not be negative"));
    }
    let reason = request
        .reason
        .clone()
        .unwrap_or_else(|| match request.state.as_str() {
            "BANNED" => "MEETING_BAN".into(),
            "LEFT" => "MEETING_LEFT".into(),
            _ => "MEETING_REMOVAL".into(),
        });
    if !membership::valid_reason(&reason) {
        return Err(Error::validation(
            "reason must be an UPPER_SNAKE_CASE code of 2-48 characters",
        ));
    }
    let st = state.clone();
    let ctx = ctx.clone();
    state
        .db
        .transaction::<_, Value, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let rctx = request_context();
                let conversation = meeting_conversation(txn, &ctx, conversation_id).await?;
                let key = format!("member:{}", principal.simple());
                if !advance_fence(txn, conversation.id, &key, SOURCE, request.source_version, now).await? {
                    crate::metrics::inc("messaging_sync_updates_total", &[("outcome", "stale")]);
                    return Ok(json!({ "applied": false, "reason": "STALE_VERSION" }));
                }
                let current = crate::conversations::access::membership(txn, conversation.id, principal, true).await?;
                let active = current.as_ref().is_some_and(|m| m.is_active());
                let owner = current.as_ref().is_some_and(|m| m.role == "OWNER");
                let mut rekey = false;
                match request.state.as_str() {
                    "ACTIVE" => {
                        membership::unban_in(txn, conversation.id, principal, now).await?;
                        if active {
                            if !owner {
                                membership::set_role_in(txn, &ctx, &conversation, principal, &role, &rctx, now).await?;
                            }
                        } else {
                            membership::add_in(
                                txn,
                                &st,
                                &ctx,
                                &Administrator::Application,
                                &conversation,
                                principal,
                                &role,
                                &rctx,
                                now,
                            )
                            .await?;
                        }
                    }
                    "BANNED" if !owner => {
                        membership::ban_in(txn, &ctx, &conversation, principal, &reason, None, &rctx, now).await?;
                        rekey = active && conversation.profile() == SecurityProfile::PrivateE2ee;
                    }
                    "REMOVED" | "LEFT" if active && !owner => {
                        let view = membership::remove_in(
                            txn,
                            &ctx,
                            &conversation,
                            principal,
                            request.state == "LEFT",
                            &reason,
                            None,
                            &rctx,
                            now,
                        )
                        .await?;
                        rekey = view["mlsRekeyRequired"] == true;
                    }
                    // The owner's access follows the conversation lifecycle, not
                    // individual leave events; an inactive member is already out.
                    _ => {}
                }
                audit::record(
                    txn,
                    AuditEntry {
                        ctx: &ctx,
                        principal_id: None,
                        action: "MEETING_MEMBER_SYNCED",
                        target_kind: "CONVERSATION",
                        target_id: conversation.id,
                        detail: json!({ "member_principal_id": principal, "state": request.state, "role": role,
                                        "source_version": request.source_version }),
                    },
                    &rctx,
                    now,
                )
                .await?;
                crate::metrics::inc("messaging_sync_updates_total", &[("outcome", "applied")]);
                let member = crate::conversations::access::membership(txn, conversation.id, principal, false).await?;
                Ok(json!({
                    "applied": true,
                    "member": member.map(|m| json!({ "principalId": principal, "state": m.state, "role": m.role })),
                    "mlsRekeyRequired": rekey,
                    "cryptographicRemoval": if conversation.profile() == SecurityProfile::PrivateE2ee && request.state != "ACTIVE" {
                        "PENDING_MEMBER_COMMIT"
                    } else {
                        "NOT_APPLICABLE"
                    },
                }))
            })
        })
        .await
        .map_err(Error::from)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LifecycleSync {
    pub lifecycle_state: LifecycleState,
    pub source_version: i64,
    #[serde(default)]
    pub reason: Option<String>,
}

/// `POST /internal/v1/meeting-conversations/{id}/lifecycle` (e.g. CLOSED when the meeting
/// ends, LOCKED while it is locked).
pub async fn sync_lifecycle(
    state: &AppState,
    ctx: &TrustedContext,
    conversation_id: Uuid,
    request: LifecycleSync,
) -> Result<Value> {
    if !request.lifecycle_state.client_requestable() {
        return Err(Error::reason(
            ErrorCode::InvalidTransition,
            "SERVER_ONLY_STATE",
        ));
    }
    let reason = request
        .reason
        .clone()
        .unwrap_or_else(|| "MEETING_LIFECYCLE".into());
    if !membership::valid_reason(&reason) {
        return Err(Error::validation(
            "reason must be an UPPER_SNAKE_CASE code of 2-48 characters",
        ));
    }
    let st = state.clone();
    let ctx = ctx.clone();
    state
        .db
        .transaction::<_, Value, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let rctx = request_context();
                let row = meeting_conversation(txn, &ctx, conversation_id).await?;
                if !advance_fence(
                    txn,
                    row.id,
                    "lifecycle",
                    SOURCE,
                    request.source_version,
                    now,
                )
                .await?
                {
                    return Ok(json!({ "applied": false, "reason": "STALE_VERSION" }));
                }
                let current = row.state();
                let next = request.lifecycle_state;
                if next != current {
                    if !current.can_become(next) {
                        return Err(Error::reason(
                            ErrorCode::InvalidTransition,
                            "TRANSITION_NOT_ALLOWED",
                        ));
                    }
                    service::transition(txn, &ctx, &row, next, Some(&reason), None, now, &rctx)
                        .await?;
                }
                Ok(json!({ "applied": true, "lifecycleState": next }))
            })
        })
        .await
        .map_err(Error::from)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MessageModeration {
    /// `REMOVE`: a moderation tombstone for every member.
    pub action: String,
    #[serde(default)]
    pub reason: Option<String>,
}

/// `POST /internal/v1/meeting-conversations/{id}/messages/{messageId}/moderation`: the
/// meeting authority (which has already checked the host's meeting role against the
/// sender's) removes a message. Idempotent; the content is tombstoned, never returned.
pub async fn moderate_message(
    state: &AppState,
    ctx: &TrustedContext,
    conversation_id: Uuid,
    message_id: Uuid,
    request: MessageModeration,
) -> Result<Value> {
    if request.action != "REMOVE" {
        return Err(Error::validation("action must be REMOVE"));
    }
    let reason = request
        .reason
        .clone()
        .unwrap_or_else(|| "MEETING_MODERATOR_REMOVED".into());
    if !membership::valid_reason(&reason) {
        return Err(Error::validation(
            "reason must be an UPPER_SNAKE_CASE code of 2-48 characters",
        ));
    }
    let effective =
        crate::conversations::policy::current(&state.db, ctx.scope.environment_id).await?;
    let grace = chrono::Duration::seconds(effective.retention.deleted_purge_grace_seconds);
    let st = state.clone();
    let ctx = ctx.clone();
    state
        .db
        .transaction::<_, Value, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let rctx = request_context();
                let conversation = meeting_conversation(txn, &ctx, conversation_id).await?;
                let row = crate::messages::load_message(txn, conversation.id, message_id, true)
                    .await?
                    .ok_or(Error::code(ErrorCode::NotFound))?;
                if row.message_kind == "MLS_COMMIT" {
                    return Err(Error::reason(ErrorCode::MessageNotModifiable, "HANDSHAKE"));
                }
                if !row.is_active() {
                    return Ok(json!({ "messageId": row.id, "lifecycle": row.lifecycle, "changed": false }));
                }
                crate::messages::tombstone(
                    txn,
                    &service::scope_of(&ctx),
                    &row,
                    "MODERATED",
                    &reason,
                    now + grace,
                    &rctx,
                    now,
                )
                .await?;
                audit::record(
                    txn,
                    AuditEntry {
                        ctx: &ctx,
                        principal_id: None,
                        action: "MEETING_MESSAGE_MODERATED",
                        target_kind: "MESSAGE",
                        target_id: row.id,
                        detail: json!({ "reason": reason }),
                    },
                    &rctx,
                    now,
                )
                .await?;
                Ok(json!({ "messageId": row.id, "lifecycle": "MODERATED", "changed": true }))
            })
        })
        .await
        .map_err(Error::from)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyMessage {
    pub legacy_message_id: String,
    pub sender_principal_id: Uuid,
    /// `None` / omitted for a message deleted at its source (imported as a tombstone).
    #[serde(default)]
    pub text: Option<String>,
    pub sent_at: DateTime<Utc>,
    #[serde(default)]
    pub presentation: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyImport {
    /// Opaque name of the legacy store (`[a-z][a-z0-9.-]{1,47}`).
    pub legacy_source: String,
    pub messages: Vec<LegacyMessage>,
}

/// `POST /internal/v1/meeting-conversations/{id}/legacy-imports`.
pub async fn import_legacy(
    state: &AppState,
    ctx: &TrustedContext,
    conversation_id: Uuid,
    request: LegacyImport,
) -> Result<Value> {
    let source_ok = (2..=48).contains(&request.legacy_source.len())
        && request.legacy_source.as_bytes()[0].is_ascii_lowercase()
        && request
            .legacy_source
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-');
    if !source_ok {
        return Err(Error::validation(
            "legacySource must be [a-z][a-z0-9.-]{1,47}",
        ));
    }
    if request.messages.is_empty() || request.messages.len() > MAX_IMPORT_BATCH {
        return Err(Error::validation(format!(
            "messages: 1-{MAX_IMPORT_BATCH} per batch"
        )));
    }
    for m in &request.messages {
        let id_ok = (1..=128).contains(&m.legacy_message_id.len())
            && m.legacy_message_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'));
        if !id_ok {
            return Err(Error::validation(
                "legacyMessageId must be 1-128 characters of [A-Za-z0-9._:-]",
            ));
        }
        if m.presentation
            .as_deref()
            .is_some_and(|p| !matches!(p, "STANDARD" | "ANNOUNCEMENT"))
        {
            return Err(Error::validation(
                "presentation must be STANDARD or ANNOUNCEMENT",
            ));
        }
    }
    managed::require_ring(state)?;
    let st = state.clone();
    let ctx = ctx.clone();
    state
        .db
        .transaction::<_, Value, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let rctx = request_context();
                let conversation = meeting_conversation(txn, &ctx, conversation_id).await?;
                if conversation.profile() != SecurityProfile::Managed {
                    // Legacy plaintext can never enter an end-to-end encrypted conversation.
                    return Err(Error::reason(ErrorCode::ContentRejected, "LEGACY_IMPORT_REQUIRES_MANAGED"));
                }
                // Serialize with senders of this conversation's sequence.
                db::exec(
                    txn,
                    "SELECT 1 FROM messaging_conversation_sequences WHERE conversation_id = $1 FOR UPDATE",
                    vec![conversation.id.into()],
                )
                .await?;
                let ring = st.managed_keys.as_deref().ok_or(Error::code(ErrorCode::FeatureDisabled))?;
                let (mut imported, mut duplicates, mut refused) = (0, 0, Vec::new());
                // legacy id -> canonical Messaging id, so the legacy system keeps a stable mapping.
                let mut items = Vec::with_capacity(request.messages.len());
                for m in &request.messages {
                    // The author must be (or have been) a member of this conversation.
                    if crate::conversations::access::membership(txn, conversation.id, m.sender_principal_id, false)
                        .await?
                        .is_none()
                    {
                        refused.push(json!({ "legacyMessageId": m.legacy_message_id, "reason": "SENDER_NOT_A_MEMBER" }));
                        continue;
                    }
                    let presentation = m.presentation.as_deref().filter(|p| *p == "ANNOUNCEMENT");
                    match managed::import_legacy(
                        txn,
                        ring,
                        &ctx,
                        &conversation,
                        m.sender_principal_id,
                        &request.legacy_source,
                        &m.legacy_message_id,
                        m.text.as_deref(),
                        presentation,
                        m.sent_at,
                        now,
                    )
                    .await?
                    {
                        Some(row) => {
                            crate::messages::record_created_notify(txn, &ctx, &row, &rctx, now, false).await?;
                            imported += 1;
                            items.push(json!({ "legacyMessageId": m.legacy_message_id, "messageId": row.id,
                                               "sequence": row.sequence, "status": "IMPORTED" }));
                        }
                        None => {
                            duplicates += 1;
                            let existing: Option<(Uuid, i64)> = db::one::<sea_orm::JsonValue, _>(
                                txn,
                                "SELECT id, sequence FROM messaging_messages WHERE conversation_id = $1 \
                                   AND legacy_source = $2 AND legacy_message_id = $3",
                                vec![conversation.id.into(), request.legacy_source.clone().into(), m.legacy_message_id.clone().into()],
                            )
                            .await?
                            .and_then(|r| {
                                Some((Uuid::parse_str(r.get("id")?.as_str()?).ok()?, r.get("sequence")?.as_i64()?))
                            });
                            if let Some((id, sequence)) = existing {
                                items.push(json!({ "legacyMessageId": m.legacy_message_id, "messageId": id,
                                                   "sequence": sequence, "status": "DUPLICATE" }));
                            }
                        }
                    }
                }
                audit::record(
                    txn,
                    AuditEntry {
                        ctx: &ctx,
                        principal_id: None,
                        action: "LEGACY_MESSAGES_IMPORTED",
                        target_kind: "CONVERSATION",
                        target_id: conversation.id,
                        detail: json!({ "legacy_source": request.legacy_source, "imported": imported,
                                        "duplicates": duplicates, "refused": refused.len() }),
                    },
                    &rctx,
                    now,
                )
                .await?;
                crate::metrics::inc("messaging_legacy_imports_total", &[]);
                Ok(json!({ "imported": imported, "duplicates": duplicates, "refused": refused,
                           "items": items, "securityProvenance": "LEGACY_MANAGED" }))
            })
        })
        .await
        .map_err(Error::from)
}
