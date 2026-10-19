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

use std::{collections::HashMap, fmt, path::Path};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chacha20poly1305::{
    aead::{Aead, KeyInit, OsRng, Payload},
    AeadCore, XChaCha20Poly1305, XNonce,
};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde_json::Value;
use uuid::Uuid;
use zeroize::Zeroizing;

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

const SEAL_LABEL: &str = "cfmeet-messaging-managed:v1";

/// The managed-content key ring (server keys; MANAGED only).
pub struct ManagedKeyring {
    active: String,
    keys: HashMap<String, Zeroizing<[u8; 32]>>,
}

impl fmt::Debug for ManagedKeyring {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ManagedKeyring")
            .field("active", &self.active)
            .field("keys", &self.keys.len())
            .finish()
    }
}

fn valid_key_id(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

impl ManagedKeyring {
    /// Parses a key-ring file: one `<key id> <base64url 32-byte key>` per line; the
    /// FIRST line is the active key. Errors never echo key material.
    pub fn parse(text: &str) -> std::result::Result<Self, String> {
        let mut keys = HashMap::new();
        let mut active = None;
        for line in text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            let (id, key) = line
                .split_once(char::is_whitespace)
                .ok_or("each line must be `<key id> <base64url key>`")?;
            if !valid_key_id(id) {
                return Err("invalid key id (1-32 of [A-Za-z0-9._-])".into());
            }
            let bytes = Zeroizing::new(
                URL_SAFE_NO_PAD
                    .decode(key.trim())
                    .map_err(|_| "keys must be unpadded base64url")?,
            );
            let key: [u8; 32] = bytes
                .as_slice()
                .try_into()
                .map_err(|_| "keys must be exactly 32 bytes")?;
            if keys.insert(id.to_string(), Zeroizing::new(key)).is_some() {
                return Err("duplicate key id".into());
            }
            active.get_or_insert_with(|| id.to_string());
        }
        Ok(Self {
            active: active.ok_or("the key ring is empty")?,
            keys,
        })
    }

    pub fn load(path: &Path) -> std::result::Result<Self, String> {
        let text = Zeroizing::new(
            std::fs::read_to_string(path).map_err(|_| "cannot be read".to_string())?,
        );
        Self::parse(&text)
    }

    /// A random single-key ring (tests and local development only).
    pub fn generate(id: &str) -> Self {
        let mut keys = HashMap::new();
        keys.insert(
            id.to_string(),
            Zeroizing::new(XChaCha20Poly1305::generate_key(&mut OsRng).into()),
        );
        Self {
            active: id.to_string(),
            keys,
        }
    }

    fn aad(row_scope: &SealScope) -> Vec<u8> {
        format!(
            "{SEAL_LABEL}:{}:{}:{}:{}:{}",
            row_scope.tenant_id,
            row_scope.application_id,
            row_scope.environment_id,
            row_scope.conversation_id,
            row_scope.message_id
        )
        .into_bytes()
    }

    fn seal(&self, scope: &SealScope, text: &str) -> Result<Sealed> {
        let key = self
            .keys
            .get(&self.active)
            .ok_or(Error::code(ErrorCode::InternalError))?;
        let cipher = XChaCha20Poly1305::new(key.as_slice().into());
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let sealed = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: text.as_bytes(),
                    aad: &Self::aad(scope),
                },
            )
            .map_err(|_| Error::code(ErrorCode::InternalError))?;
        Ok(Sealed {
            sealed,
            nonce: nonce.to_vec(),
            key_id: self.active.clone(),
        })
    }

    /// Seals a server-held record other than message text (e.g. evidence a reporter
    /// voluntarily disclosed) under the active key, bound to `aad`.
    /// Returns `(sealed, nonce, key id)`.
    pub fn seal_record(&self, aad: &[u8], data: &[u8]) -> Result<(Vec<u8>, Vec<u8>, String)> {
        let key = self
            .keys
            .get(&self.active)
            .ok_or(Error::code(ErrorCode::InternalError))?;
        let cipher = XChaCha20Poly1305::new(key.as_slice().into());
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let sealed = cipher
            .encrypt(&nonce, Payload { msg: data, aad })
            .map_err(|_| Error::code(ErrorCode::InternalError))?;
        Ok((sealed, nonce.to_vec(), self.active.clone()))
    }

    pub fn open_record(
        &self,
        aad: &[u8],
        sealed: &[u8],
        nonce: &[u8],
        key_id: &str,
    ) -> Option<Vec<u8>> {
        let key = self.keys.get(key_id)?;
        let nonce: [u8; 24] = nonce.try_into().ok()?;
        XChaCha20Poly1305::new(key.as_slice().into())
            .decrypt(&XNonce::from(nonce), Payload { msg: sealed, aad })
            .ok()
    }

    fn open(&self, scope: &SealScope, sealed: &[u8], nonce: &[u8], key_id: &str) -> Option<String> {
        let key = self.keys.get(key_id)?;
        let nonce: [u8; 24] = nonce.try_into().ok()?;
        let cipher = XChaCha20Poly1305::new(key.as_slice().into());
        let plain = cipher
            .decrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: sealed,
                    aad: &Self::aad(scope),
                },
            )
            .ok()?;
        String::from_utf8(plain).ok()
    }
}

struct SealScope {
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    conversation_id: Uuid,
    message_id: Uuid,
}

struct Sealed {
    sealed: Vec<u8>,
    nonce: Vec<u8>,
    key_id: String,
}

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
/// cannot open it).
pub fn text_of(state: &AppState, row: &MessageRow) -> Option<String> {
    if row.profile() != SecurityProfile::Managed || !row.is_active() {
        return None;
    }
    let ring = state.managed_keys.as_ref()?;
    ring.open(
        &scope_of_row(row),
        row.managed_sealed.as_deref()?,
        row.managed_seal_nonce.as_deref()?,
        row.managed_seal_key_id.as_deref()?,
    )
}

/// The stored text of a MANAGED message whatever its lifecycle (moderation review of
/// reported content; `None` once purged). Never used for member-facing reads.
pub fn stored_text_of(state: &AppState, row: &MessageRow) -> Option<String> {
    if row.profile() != SecurityProfile::Managed {
        return None;
    }
    state.managed_keys.as_ref()?.open(
        &scope_of_row(row),
        row.managed_sealed.as_deref()?,
        row.managed_seal_nonce.as_deref()?,
        row.managed_seal_key_id.as_deref()?,
    )
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
    let sealed = ring.seal(
        &SealScope {
            tenant_id: ctx.scope.tenant_id,
            application_id: ctx.scope.application_id,
            environment_id: ctx.scope.environment_id,
            conversation_id: conversation.id,
            message_id: id,
        },
        content.text(),
    )?;
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

fn same_text(state: &AppState, existing: &MessageRow, text: &str) -> Result<()> {
    match text_of(state, existing) {
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
        same_text(state, &existing, content.text())?;
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
                super::lock_sender(txn, &conversation, acting.principal_id, now).await?;
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
}

/// Edits a MANAGED message in place (sender only, within the policy's window): the
/// sealed text and the search index are replaced together, and `message.edited` is
/// recorded in the same transaction.
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
                super::lock_sender(txn, &conversation, acting.principal_id, now).await?;
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
                let sealed = ring.seal(&scope_of_row(&row), content.text())?;
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

/// Imports one legacy chat message (Part V H §101) inside the caller's transaction:
/// provenance `LEGACY_MANAGED`, the ORIGINAL time kept, idempotent per (conversation,
/// legacy source, legacy id) — `Ok(None)` for an already imported message. A legacy
/// message deleted at its source is imported as a tombstone without content.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn import_legacy<C: ConnectionTrait>(
    db: &C,
    ring: &ManagedKeyring,
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
        Some(c) => Some(ring.seal(
            &SealScope {
                tenant_id: ctx.scope.tenant_id,
                application_id: ctx.scope.application_id,
                environment_id: ctx.scope.environment_id,
                conversation_id: conversation.id,
                message_id: id,
            },
            c.text(),
        )?),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_ring_parses_strictly_and_never_echoes_keys() {
        let key = URL_SAFE_NO_PAD.encode([7u8; 32]);
        let ring = ManagedKeyring::parse(&format!(
            "# ring\nk2 {key}\nk1 {}\n",
            URL_SAFE_NO_PAD.encode([8u8; 32])
        ))
        .unwrap();
        assert_eq!(ring.active, "k2");
        assert!(!format!("{ring:?}").contains(&key));
        for bad in [
            "",
            "k1",
            "k1 short",
            "bad id! xx",
            &format!("k1 {key}\nk1 {key}"),
        ] {
            let err = ManagedKeyring::parse(bad).unwrap_err();
            assert!(!err.contains(&key));
        }
        let scope = SealScope {
            tenant_id: Uuid::new_v4(),
            application_id: Uuid::new_v4(),
            environment_id: Uuid::new_v4(),
            conversation_id: Uuid::new_v4(),
            message_id: Uuid::new_v4(),
        };
        let s = ring.seal(&scope, "hello managed").unwrap();
        assert!(!s.sealed.windows(5).any(|w| w == b"hello"));
        assert_eq!(
            ring.open(&scope, &s.sealed, &s.nonce, &s.key_id).as_deref(),
            Some("hello managed")
        );
        // Bound to the message: another message id cannot open it.
        let moved = SealScope {
            message_id: Uuid::new_v4(),
            ..scope
        };
        assert_eq!(ring.open(&moved, &s.sealed, &s.nonce, &s.key_id), None);
    }
}
