//! Attachments and media messages (Part V Part F, §69–§80).
//!
//! * Bytes live in protected object storage (`packages/object-storage`, private bucket);
//!   PostgreSQL holds only authorized metadata and the SERVER-GENERATED object key
//!   (`att/<environment>/<conversation>/<attachment>`) — an untrusted file name never
//!   becomes a key.
//! * Upload intent → signed single-object upload bound to the exact size, SHA-256 and
//!   media type (the store rejects anything else) → finalization re-verifies size and
//!   digest against the stored object → READY (or QUARANTINED / REJECTED). Only READY
//!   attachments of the sender, in the same conversation, can be referenced by a message.
//! * PRIVATE_E2EE (AD-08): the client encrypted the file BEFORE upload
//!   (`cf_messaging_crypto::attachment`, client feature) and sends the key only inside the
//!   MLS-protected message. The server verifies ciphertext-object properties (size,
//!   digest, its scope) and NOTHING else: no file name, no media metadata, no format
//!   checks, no malware scan — and it never claims one (`NOT_APPLICABLE_ENCRYPTED`).
//! * MANAGED: content-type security ([`content`]) and fail-closed self-hosted scanning
//!   ([`scanner`]) under policy.
//! * Quotas ([`quota`]) are reserved atomically at intent time and settled on
//!   finalization / expiry; client-reported totals are never trusted.
//! * Downloads: authorization re-checked on every grant (current membership, the
//!   member's history window, the message still ACTIVE, the attachment READY); grants
//!   are short-lived signed reads forcing safe response headers, or a proxied read that
//!   re-checks authorization on each request (strong revocation).
//! * Lifecycle workers ([`lifecycle`]): expired intents, abandoned uploads, scan retries,
//!   the object-deletion queue (legal holds respected) and orphan reconciliation.
//! * Media processing ([`processing`]): the asynchronous derivative contract (thumbnails,
//!   transcodes, waveforms …) — MANAGED only; E2EE derivatives are client-created.

pub mod content;
pub mod derivatives;
pub mod http;
pub mod lifecycle;
pub mod media_inspect;
pub mod processing;
pub mod quota;
pub mod scanner;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    conversations::{
        access,
        policy::{self, EffectivePolicy},
        service::{request_context, rfc3339, ConversationRow},
        ContextClass,
    },
    db,
    domain::SecurityProfile,
    error::{Error, ErrorCode, Result},
    http::middleware::Acting,
    identity::TrustedContext,
    limits::{self, Window},
    state::AppState,
};

/// The only accepted client encryption format of E2EE attachments.
pub const E2EE_FORMAT_V1: &str = "CFMEET_ATTACHMENT_XCHACHA20POLY1305_STREAM_V1";
/// Ciphertext overhead allowance of an E2EE upload over the plaintext policy limit
/// (header + one 16-byte tag per 64 KiB chunk, rounded up generously).
fn e2ee_allowance(max_plain: i64) -> i64 {
    max_plain + max_plain / 4096 + 4096
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct AttachmentRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub conversation_id: Uuid,
    pub uploader_principal_id: Uuid,
    pub uploader_device_id: Option<Uuid>,
    pub security_profile: String,
    pub kind: String,
    pub state: String,
    pub state_reason: Option<String>,
    pub object_key: String,
    pub declared_size: i64,
    pub verified_size: Option<i64>,
    pub declared_sha256: String,
    pub media_type: String,
    pub safe_file_name: Option<String>,
    pub media_metadata: Option<Value>,
    pub crypto_format: Option<String>,
    pub scan_status: String,
    pub scan_engine: Option<String>,
    pub scan_finding: Option<String>,
    pub scan_attempts: i32,
    pub reserved_bytes: i64,
    pub committed_bytes: i64,
    pub message_id: Option<Uuid>,
    pub intent_expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub finalized_at: Option<DateTime<Utc>>,
}

pub const ATTACHMENT_COLUMNS: &str = "id, tenant_id, application_id, environment_id, conversation_id, uploader_principal_id, \
     uploader_device_id, security_profile, kind, state, state_reason, object_key, declared_size, verified_size, \
     declared_sha256, media_type, safe_file_name, media_metadata, crypto_format, scan_status, scan_engine, scan_finding, \
     scan_attempts, reserved_bytes, committed_bytes, message_id, intent_expires_at, created_at, finalized_at";

impl AttachmentRow {
    pub fn profile(&self) -> SecurityProfile {
        SecurityProfile::parse(&self.security_profile).unwrap_or(SecurityProfile::PrivateE2ee)
    }
}

pub fn view(a: &AttachmentRow) -> Value {
    let e2ee = a.profile() == SecurityProfile::PrivateE2ee;
    json!({
        "attachmentId": a.id,
        "conversationId": a.conversation_id,
        "kind": a.kind,
        "state": a.state,
        "stateReason": a.state_reason,
        "securityProfile": a.security_profile,
        "uploaderPrincipalId": a.uploader_principal_id,
        "size": a.declared_size,
        "sha256": a.declared_sha256,
        "mediaType": a.media_type,
        "fileName": a.safe_file_name,
        "media": if e2ee { Value::Null } else { a.media_metadata.clone().unwrap_or(Value::Null) },
        "cryptoFormat": a.crypto_format,
        "scan": {
            "status": a.scan_status,
            "engine": a.scan_engine,
            "finding": if a.state == "QUARANTINED" { a.scan_finding.clone() } else { None },
        },
        "messageId": a.message_id,
        "createdAt": rfc3339(a.created_at),
        "finalizedAt": a.finalized_at.map(rfc3339),
        "intentExpiresAt": rfc3339(a.intent_expires_at),
    })
}

pub async fn load<C: ConnectionTrait>(
    db: &C,
    conversation_id: Uuid,
    id: Uuid,
    lock: bool,
) -> Result<Option<AttachmentRow>> {
    Ok(db::one(
        db,
        &format!(
            "SELECT {ATTACHMENT_COLUMNS} FROM messaging_attachments WHERE id = $1 AND conversation_id = $2{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        vec![id.into(), conversation_id.into()],
    )
    .await?)
}

/// Whether attachments may be used in this conversation's context (environment and
/// context-class policy).
fn attachments_enabled(policy: &EffectivePolicy, conversation: &ConversationRow) -> bool {
    let class = ContextClass::parse(&conversation.context_class);
    let context = class
        .and_then(|c| policy.contexts.get(&c))
        .and_then(|c| c.attachments_enabled);
    policy.attachments.enabled && context != Some(false)
}

pub(crate) fn require_storage(state: &AppState) -> Result<()> {
    if state.storage.configured() {
        Ok(())
    } else {
        Err(Error::reason(
            ErrorCode::FeatureDisabled,
            "ATTACHMENT_STORAGE_NOT_CONFIGURED",
        ))
    }
}

fn storage_error(e: cf_object_storage::StorageError) -> Error {
    match e {
        cf_object_storage::StorageError::NotConfigured => Error::reason(
            ErrorCode::FeatureDisabled,
            "ATTACHMENT_STORAGE_NOT_CONFIGURED",
        ),
        _ => Error::code(ErrorCode::StorageUnavailable),
    }
}

// ---------------------------------------------------------------------------------------
// Upload intent
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IntentRequest {
    pub kind: String,
    /// Exact size of the bytes that will be uploaded (ciphertext for PRIVATE_E2EE).
    pub size: i64,
    /// Lower-case hex SHA-256 of those bytes.
    pub sha256: String,
    /// MANAGED: the media type. PRIVATE_E2EE: omitted (protected inside MLS).
    #[serde(default)]
    pub media_type: Option<String>,
    /// MANAGED only (presentation; sanitized).
    #[serde(default)]
    pub file_name: Option<String>,
    /// MANAGED voice notes / videos: declared duration (unverified by the server).
    #[serde(default)]
    pub duration_ms: Option<i64>,
    #[serde(default)]
    pub codec: Option<String>,
    /// MANAGED voice notes: optional client-generated waveform (≤ 256 samples 0–255).
    #[serde(default)]
    pub waveform: Option<Vec<u8>>,
    /// PRIVATE_E2EE: the uploading device and the client encryption format.
    #[serde(default)]
    pub device_id: Option<Uuid>,
    #[serde(default)]
    pub crypto_format: Option<String>,
}

fn valid_sha(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_codec(c: &str) -> bool {
    (1..=64).contains(&c.len())
        && c.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'/'))
}

pub fn object_key(environment: Uuid, conversation: Uuid, attachment: Uuid) -> String {
    format!(
        "att/{}/{}/{}",
        environment.simple(),
        conversation.simple(),
        attachment.simple()
    )
}

fn upload_view(w: &cf_object_storage::SignedWrite) -> Value {
    json!({
        "method": w.method,
        "url": w.url,
        "headers": w.headers.iter().map(|(k, v)| json!({ "name": k, "value": v })).collect::<Vec<_>>(),
        "expiresAt": rfc3339(w.expires_at),
    })
}

/// `POST /v1/conversations/{id}/attachments` — an upload intent.
pub async fn create_intent(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    request: IntentRequest,
) -> Result<(u16, Value)> {
    require_storage(state)?;
    crate::operations::require_open(
        &state.db,
        crate::operations::KillSwitch::AttachmentUpload,
        ctx.scope.environment_id,
    )
    .await?;
    let now = state.now();
    let (conversation, member) =
        access::as_member(&state.db, ctx, conversation_id, acting.principal_id).await?;
    access::require_accepting(&conversation, now)?;
    if !member.may_send(now) {
        return Err(Error::reason(ErrorCode::SendRestricted, "MUTED"));
    }
    let effective = policy::current(&state.db, ctx.scope.environment_id).await?;
    if !attachments_enabled(&effective, &conversation) {
        return Err(Error::reason(
            ErrorCode::FeatureDisabled,
            "ATTACHMENTS_DISABLED",
        ));
    }
    let a = &effective.attachments;
    if !a.allowed_kinds.contains(&request.kind) {
        return Err(Error::reason(
            ErrorCode::ContentRejected,
            "KIND_NOT_ALLOWED",
        ));
    }
    if !valid_sha(&request.sha256) {
        return Err(Error::validation(
            "sha256 must be 64 lower-case hex characters",
        ));
    }
    let profile = conversation.profile();
    let limit = match profile {
        SecurityProfile::PrivateE2ee => e2ee_allowance(a.max_bytes),
        SecurityProfile::Managed => a.max_bytes,
    };
    if request.size < 1 || request.size > limit {
        return Err(Error::reason(
            ErrorCode::QuotaExceeded,
            "ATTACHMENT_TOO_LARGE",
        ));
    }
    // What the server may know depends on the PERSISTED profile.
    let (media_type, file_name, metadata, device, format) = match profile {
        SecurityProfile::PrivateE2ee => {
            if request.file_name.is_some()
                || request.duration_ms.is_some()
                || request.codec.is_some()
                || request.waveform.is_some()
                || request
                    .media_type
                    .as_deref()
                    .is_some_and(|m| m != "application/octet-stream")
            {
                // Names, media types and durations of E2EE files are protected content:
                // they travel only inside the MLS-encrypted attachment descriptor.
                return Err(Error::reason(
                    ErrorCode::ContentRejected,
                    "PROTECTED_METADATA_IN_E2EE",
                ));
            }
            let device = request
                .device_id
                .ok_or_else(|| Error::validation("PRIVATE_E2EE uploads need deviceId"))?;
            crate::messages::e2ee::require_own_device(&state.db, ctx, acting.principal_id, device)
                .await?;
            if request.crypto_format.as_deref() != Some(E2EE_FORMAT_V1) {
                return Err(Error::reason(
                    ErrorCode::ContentRejected,
                    "UNSUPPORTED_CRYPTO_FORMAT",
                ));
            }
            (
                "application/octet-stream".to_string(),
                None,
                None,
                Some(device),
                Some(E2EE_FORMAT_V1),
            )
        }
        SecurityProfile::Managed => {
            if request.crypto_format.is_some() || request.device_id.is_some() {
                return Err(Error::reason(
                    ErrorCode::ContentRejected,
                    "E2EE_FIELDS_IN_MANAGED",
                ));
            }
            let media_type = request
                .media_type
                .clone()
                .ok_or_else(|| Error::validation("MANAGED uploads need mediaType"))?;
            if !content::allowed_media_types(&request.kind).contains(&media_type.as_str()) {
                return Err(Error::reason(
                    ErrorCode::ContentRejected,
                    "MEDIA_TYPE_NOT_ALLOWED",
                ));
            }
            let file_name = match &request.file_name {
                Some(n) => Some(content::sanitize_file_name(n).ok_or_else(|| {
                    Error::reason(ErrorCode::ContentRejected, "INVALID_FILE_NAME")
                })?),
                None => None,
            };
            if let Some(n) = &file_name {
                if n.split('.').skip(1).any(|e| {
                    content::PROHIBITED_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str())
                }) {
                    return Err(Error::reason(
                        ErrorCode::ContentRejected,
                        "PROHIBITED_FILE_NAME",
                    ));
                }
            }
            let mut media = serde_json::Map::new();
            match request.kind.as_str() {
                "VOICE_NOTE" | "VIDEO" => {
                    let max = if request.kind == "VOICE_NOTE" {
                        a.max_voice_note_seconds
                    } else {
                        a.max_video_seconds
                    };
                    let d = request.duration_ms.ok_or_else(|| {
                        Error::validation("voice notes and videos declare durationMs")
                    })?;
                    if d < 1 || d > max * 1000 {
                        return Err(Error::reason(
                            ErrorCode::ContentRejected,
                            "DURATION_NOT_ALLOWED",
                        ));
                    }
                    media.insert("durationMs".into(), json!(d));
                    media.insert("durationVerified".into(), json!(false));
                }
                _ if request.duration_ms.is_some() => {
                    return Err(Error::validation(
                        "durationMs applies to voice notes and videos",
                    ));
                }
                _ => {}
            }
            if let Some(c) = &request.codec {
                if !valid_codec(c) {
                    return Err(Error::validation(
                        "codec must be 1-64 characters of [A-Za-z0-9./_-]",
                    ));
                }
                media.insert("codec".into(), json!(c));
            }
            if let Some(w) = &request.waveform {
                if request.kind != "VOICE_NOTE" || w.len() > 256 {
                    return Err(Error::validation(
                        "waveform: voice notes only, at most 256 samples",
                    ));
                }
                media.insert("waveform".into(), json!(w));
            }
            (
                media_type,
                file_name,
                (!media.is_empty()).then_some(Value::Object(media)),
                None,
                None,
            )
        }
    };
    let key = format!(
        "uploads:{}:{}",
        ctx.scope.environment_id.simple(),
        acting.principal_id.simple()
    );
    if let Some(retry) = limits::hit(
        &state.db,
        &key,
        Window::Hour,
        effective.abuse.uploads_per_hour,
        now,
    )
    .await?
    {
        crate::safety::abuse::signal(state, ctx, acting.principal_id, "UPLOAD_VELOCITY").await;
        return Err(Error::RateLimited {
            retry_after_seconds: retry,
        });
    }
    // Idempotent: the same pending upload (uploader, conversation, digest, size) is reused.
    let existing: Option<AttachmentRow> = db::one(
        &state.db,
        &format!(
            "SELECT {ATTACHMENT_COLUMNS} FROM messaging_attachments WHERE conversation_id = $1 AND uploader_principal_id = $2 \
               AND declared_sha256 = $3 AND declared_size = $4 AND state = 'INTENT_CREATED' AND intent_expires_at > $5 LIMIT 1"
        ),
        vec![conversation.id.into(), acting.principal_id.into(), request.sha256.clone().into(), request.size.into(), now.into()],
    )
    .await?;
    if let Some(row) = existing {
        let w = signed_write(state, &row, now)?;
        let mut v = view(&row);
        v["upload"] = upload_view(&w);
        return Ok((200, v));
    }
    let id = Uuid::new_v4();
    let object = object_key(ctx.scope.environment_id, conversation.id, id);
    let expires = now + state.attachments.intent_ttl;
    let st = state.clone();
    let ctx2 = ctx.clone();
    let acting2 = acting.clone();
    let req = request.clone();
    let budget_policy = effective.attachments.clone();
    let row = state
        .db
        .transaction::<_, AttachmentRow, Error>(move |txn| {
            Box::pin(async move {
                quota::reserve(txn, &st, &ctx2, acting2.principal_id, req.size, &budget_policy, now).await?;
                db::exec(
                    txn,
                    "INSERT INTO messaging_attachments (id, tenant_id, application_id, environment_id, conversation_id, \
                       uploader_principal_id, uploader_device_id, security_profile, kind, object_key, declared_size, \
                       declared_sha256, media_type, safe_file_name, media_metadata, crypto_format, scan_status, \
                       reserved_bytes, intent_expires_at, created_at, state_changed_at) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $11, $18, $19, $19)",
                    vec![
                        id.into(),
                        ctx2.scope.tenant_id.into(),
                        ctx2.scope.application_id.into(),
                        ctx2.scope.environment_id.into(),
                        conversation.id.into(),
                        acting2.principal_id.into(),
                        device.into(),
                        profile.as_str().into(),
                        req.kind.clone().into(),
                        object.clone().into(),
                        req.size.into(),
                        req.sha256.clone().into(),
                        media_type.clone().into(),
                        file_name.clone().into(),
                        metadata.clone().into(),
                        format.map(str::to_string).into(),
                        (if profile == SecurityProfile::PrivateE2ee {
                            "NOT_APPLICABLE_ENCRYPTED"
                        } else if budget_policy.managed_scan_required {
                            "PENDING"
                        } else {
                            "NOT_REQUIRED"
                        })
                        .into(),
                        expires.into(),
                        now.into(),
                    ],
                )
                .await?;
                load(txn, conversation.id, id, false)
                    .await?
                    .ok_or(Error::code(ErrorCode::InternalError))
            })
        })
        .await
        .map_err(Error::from)?;
    let w = signed_write(state, &row, now)?;
    crate::metrics::inc(
        "messaging_attachment_intents_total",
        &[(
            "profile",
            if profile == SecurityProfile::Managed {
                "managed"
            } else {
                "e2ee"
            },
        )],
    );
    let mut v = view(&row);
    v["upload"] = upload_view(&w);
    Ok((201, v))
}

fn signed_write(
    state: &AppState,
    row: &AttachmentRow,
    now: DateTime<Utc>,
) -> Result<cf_object_storage::SignedWrite> {
    let ttl = (row.intent_expires_at - now).to_std().unwrap_or_default();
    state
        .storage
        .signed_write(
            &row.object_key,
            ttl,
            now,
            row.declared_size as u64,
            &row.declared_sha256,
            &row.media_type,
        )
        .map_err(storage_error)
}

// ---------------------------------------------------------------------------------------
// Finalization
// ---------------------------------------------------------------------------------------

/// Settles an attachment's terminal / verifying state in one transaction (guarded by its
/// current state, so concurrent finalizations have one winner).
#[allow(clippy::too_many_arguments)]
async fn settle(
    state: &AppState,
    row: &AttachmentRow,
    from: &'static str,
    to: &'static str,
    reason: Option<&'static str>,
    scan_status: &str,
    scan_finding: Option<String>,
    next_scan: Option<DateTime<Utc>>,
) -> Result<bool> {
    let st = state.clone();
    let row = row.clone();
    let scan_status = scan_status.to_string();
    let engine = state.scanner.engine();
    state
        .db
        .transaction::<_, bool, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let n = db::exec(
                    txn,
                    "UPDATE messaging_attachments SET state = $3, state_reason = $4, scan_status = $5, scan_finding = $6, \
                       scan_engine = CASE WHEN $5 IN ('CLEAN', 'INFECTED') THEN $7 ELSE scan_engine END, \
                       scan_attempts = scan_attempts + CASE WHEN $5 IN ('CLEAN', 'INFECTED', 'PENDING') AND $9 THEN 1 ELSE 0 END, \
                       next_scan_at = $8, verified_size = CASE WHEN $3 IN ('READY', 'QUARANTINED', 'VERIFYING') THEN declared_size ELSE verified_size END, \
                       finalized_at = COALESCE(finalized_at, $10), state_changed_at = $10 \
                     WHERE id = $1 AND state = $2",
                    vec![
                        row.id.into(),
                        from.into(),
                        to.into(),
                        reason.into(),
                        scan_status.clone().into(),
                        scan_finding.into(),
                        engine.into(),
                        next_scan.into(),
                        (from == "VERIFYING" || to != "VERIFYING").into(),
                        now.into(),
                    ],
                )
                .await?;
                if n != 1 {
                    return Ok(false);
                }
                match to {
                    // The object is stored for good: reservation becomes committed usage.
                    "READY" | "QUARANTINED" => quota::commit(txn, &row, now).await?,
                    "REJECTED" | "EXPIRED" => {
                        quota::release(txn, &row, now).await?;
                        lifecycle::enqueue_deletion(txn, &row.object_key, row.environment_id, Some(row.id), reason.unwrap_or("REJECTED"), now)
                            .await?;
                    }
                    _ => {}
                }
                Ok(true)
            })
        })
        .await
        .map_err(Error::from)
}

/// Verifies the stored object (size and SHA-256) and, for MANAGED, its content and scan.
pub(crate) async fn verify_and_settle(state: &AppState, row: &AttachmentRow) -> Result<()> {
    let store = &state.storage;
    let meta = store.head(&row.object_key).await.map_err(storage_error)?;
    let Some(meta) = meta else {
        return Err(Error::reason(
            ErrorCode::AttachmentNotReady,
            "UPLOAD_MISSING",
        ));
    };
    if meta.size != row.declared_size as u64 {
        settle(
            state,
            row,
            "INTENT_CREATED",
            "REJECTED",
            Some("SIZE_MISMATCH"),
            &row.scan_status,
            None,
            None,
        )
        .await?;
        return Err(Error::reason(
            ErrorCode::IntegrityCheckFailed,
            "SIZE_MISMATCH",
        ));
    }
    let managed = row.profile() == SecurityProfile::Managed;
    let mut bytes: Option<Vec<u8>> = None;
    let needs_bytes = managed;
    let digest_ok = match store
        .stored_sha256(&row.object_key)
        .await
        .map_err(storage_error)?
    {
        Some(d) => d == row.declared_sha256,
        None => {
            if (row.declared_size as usize) > state.attachments.verify_read_max_bytes
                && !needs_bytes
            {
                settle(
                    state,
                    row,
                    "INTENT_CREATED",
                    "REJECTED",
                    Some("INTEGRITY_UNVERIFIABLE"),
                    &row.scan_status,
                    None,
                    None,
                )
                .await?;
                return Err(Error::reason(
                    ErrorCode::IntegrityCheckFailed,
                    "INTEGRITY_UNVERIFIABLE",
                ));
            }
            let b = store
                .get(
                    &row.object_key,
                    state
                        .attachments
                        .verify_read_max_bytes
                        .max(state.attachments.managed_scan_max_bytes),
                )
                .await
                .map_err(storage_error)?
                .ok_or(Error::reason(
                    ErrorCode::AttachmentNotReady,
                    "UPLOAD_MISSING",
                ))?;
            let ok = cf_object_storage::sha256_hex(&b) == row.declared_sha256;
            bytes = Some(b);
            ok
        }
    };
    if !digest_ok {
        settle(
            state,
            row,
            "INTENT_CREATED",
            "REJECTED",
            Some("DIGEST_MISMATCH"),
            &row.scan_status,
            None,
            None,
        )
        .await?;
        return Err(Error::reason(
            ErrorCode::IntegrityCheckFailed,
            "DIGEST_MISMATCH",
        ));
    }
    if !managed {
        // PRIVATE_E2EE: the object is ciphertext of the declared size and digest in this
        // conversation's namespace. Nothing about its plaintext is (or can be) checked.
        settle(
            state,
            row,
            "INTENT_CREATED",
            "READY",
            None,
            "NOT_APPLICABLE_ENCRYPTED",
            None,
            None,
        )
        .await?;
        return Ok(());
    }
    // MANAGED: the server may inspect the bytes under policy.
    let effective = policy::current(&state.db, row.environment_id).await?;
    let scan_required =
        row.scan_status != "NOT_REQUIRED" && effective.attachments.managed_scan_required;
    if (row.declared_size as usize) > state.attachments.managed_scan_max_bytes && scan_required {
        settle(
            state,
            row,
            "INTENT_CREATED",
            "REJECTED",
            Some("TOO_LARGE_TO_SCAN"),
            "FAILED",
            None,
            None,
        )
        .await?;
        return Err(Error::reason(
            ErrorCode::ContentRejected,
            "TOO_LARGE_TO_SCAN",
        ));
    }
    let bytes = match bytes {
        Some(b) => b,
        None => store
            .get(
                &row.object_key,
                state
                    .attachments
                    .managed_scan_max_bytes
                    .max(row.declared_size as usize),
            )
            .await
            .map_err(storage_error)?
            .ok_or(Error::reason(
                ErrorCode::AttachmentNotReady,
                "UPLOAD_MISSING",
            ))?,
    };
    if cf_object_storage::sha256_hex(&bytes) != row.declared_sha256 {
        settle(
            state,
            row,
            "INTENT_CREATED",
            "REJECTED",
            Some("DIGEST_MISMATCH"),
            &row.scan_status,
            None,
            None,
        )
        .await?;
        return Err(Error::reason(
            ErrorCode::IntegrityCheckFailed,
            "DIGEST_MISMATCH",
        ));
    }
    if let Err(reason) = content::check_managed(
        &row.kind,
        &row.media_type,
        row.safe_file_name.as_deref(),
        &bytes,
    ) {
        settle(
            state,
            row,
            "INTENT_CREATED",
            "REJECTED",
            Some(reason),
            &row.scan_status,
            None,
            None,
        )
        .await?;
        crate::metrics::inc(
            "messaging_attachments_rejected_total",
            &[("reason", reason)],
        );
        return Err(Error::reason(ErrorCode::ContentRejected, reason));
    }
    if !scan_required {
        settle(
            state,
            row,
            "INTENT_CREATED",
            "READY",
            None,
            "NOT_REQUIRED",
            None,
            None,
        )
        .await?;
        return Ok(());
    }
    scan_and_settle(state, row, "INTENT_CREATED", &bytes).await
}

/// Runs the scanner and settles: CLEAN → READY, INFECTED → QUARANTINED, unavailable →
/// VERIFYING (retried; never downloadable).
pub(crate) async fn scan_and_settle(
    state: &AppState,
    row: &AttachmentRow,
    from: &'static str,
    bytes: &[u8],
) -> Result<()> {
    use scanner::ScanOutcome;
    match state.scanner.scan(bytes).await {
        ScanOutcome::Clean => {
            settle(state, row, from, "READY", None, "CLEAN", None, None).await?;
            crate::metrics::inc("messaging_attachment_scans_total", &[("result", "clean")]);
        }
        ScanOutcome::Infected(name) => {
            settle(
                state,
                row,
                from,
                "QUARANTINED",
                Some("MALWARE_DETECTED"),
                "INFECTED",
                Some(name),
                None,
            )
            .await?;
            crate::metrics::inc(
                "messaging_attachment_scans_total",
                &[("result", "infected")],
            );
        }
        ScanOutcome::Unavailable(_) => {
            let delay = chrono::Duration::seconds(30 * (1i64 << row.scan_attempts.clamp(0, 6)));
            settle(
                state,
                row,
                from,
                "VERIFYING",
                Some("SCAN_PENDING"),
                "PENDING",
                None,
                Some(state.now() + delay),
            )
            .await?;
            crate::metrics::inc(
                "messaging_attachment_scans_total",
                &[("result", "unavailable")],
            );
        }
    }
    Ok(())
}

/// `POST /v1/conversations/{id}/attachments/{attachmentId}/finalize` (uploader only).
pub async fn finalize(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    attachment_id: Uuid,
) -> Result<Value> {
    require_storage(state)?;
    access::as_member(&state.db, ctx, conversation_id, acting.principal_id).await?;
    let row = load(&state.db, conversation_id, attachment_id, false)
        .await?
        .filter(|r| r.uploader_principal_id == acting.principal_id)
        .ok_or(Error::code(ErrorCode::NotFound))?;
    match row.state.as_str() {
        "INTENT_CREATED" if row.intent_expires_at <= state.now() => {
            settle(
                state,
                &row,
                "INTENT_CREATED",
                "EXPIRED",
                Some("INTENT_EXPIRED"),
                &row.scan_status,
                None,
                None,
            )
            .await?;
            return Err(Error::reason(
                ErrorCode::AttachmentNotReady,
                "INTENT_EXPIRED",
            ));
        }
        "INTENT_CREATED" => verify_and_settle(state, &row).await?,
        // Already finalized (or being scanned): finalization is idempotent.
        _ => {}
    }
    let row = load(&state.db, conversation_id, attachment_id, false)
        .await?
        .ok_or(Error::code(ErrorCode::NotFound))?;
    if let Some(bytes) = row.verified_size {
        crate::usage::record(
            &state.db,
            crate::usage::Owner {
                tenant_id: row.tenant_id,
                application_id: row.application_id,
                environment_id: row.environment_id,
            },
            crate::usage::ATTACHMENT_STORED,
            bytes,
            row.id,
            Some(&row.security_profile),
            &format!("attachment:{}", row.id),
            state.now(),
        )
        .await?;
    }
    crate::metrics::inc("messaging_attachments_finalized_total", &[]);
    Ok(view(&row))
}

/// `GET /v1/conversations/{id}/attachments/{attachmentId}` — metadata and lifecycle.
pub async fn get(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    attachment_id: Uuid,
) -> Result<Value> {
    let row = authorize_metadata(state, ctx, acting, conversation_id, attachment_id).await?;
    let mut v = view(&row);
    // Server-made derivatives exist only for MANAGED attachments.
    v["derivatives"] = derivatives::views(state, row.id).await?;
    Ok(v)
}

async fn authorize_metadata(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    attachment_id: Uuid,
) -> Result<AttachmentRow> {
    let (_, member) =
        access::as_member(&state.db, ctx, conversation_id, acting.principal_id).await?;
    let row = load(&state.db, conversation_id, attachment_id, false)
        .await?
        .ok_or(Error::code(ErrorCode::NotFound))?;
    if row.uploader_principal_id == acting.principal_id {
        return Ok(row);
    }
    // Others see an attachment only through a message within their history window.
    let Some(mid) = row.message_id else {
        return Err(Error::code(ErrorCode::NotFound));
    };
    let msg = crate::messages::load_message(&state.db, conversation_id, mid, false)
        .await?
        .ok_or(Error::code(ErrorCode::NotFound))?;
    if msg.sequence < member.visible_from_sequence {
        return Err(Error::code(ErrorCode::NotFound));
    }
    Ok(row)
}

/// Current authorization to READ an attachment's bytes.
pub(crate) async fn authorize_read(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    attachment_id: Uuid,
) -> Result<AttachmentRow> {
    let row = authorize_metadata(state, ctx, acting, conversation_id, attachment_id).await?;
    if row.state != "READY" {
        return Err(Error::reason(
            ErrorCode::AttachmentNotReady,
            match row.state.as_str() {
                "QUARANTINED" => "QUARANTINED",
                "VERIFYING" => "SCAN_PENDING",
                "INTENT_CREATED" => "NOT_FINALIZED",
                "DELETED" => "DELETED",
                _ => "NOT_AVAILABLE",
            },
        ));
    }
    if let Some(mid) = row.message_id {
        let msg = crate::messages::load_message(&state.db, conversation_id, mid, false)
            .await?
            .ok_or(Error::code(ErrorCode::NotFound))?;
        if !msg.is_active() {
            // Deleted / moderated / expired / purged messages release no bytes.
            return Err(Error::reason(
                ErrorCode::AttachmentNotReady,
                "MESSAGE_NOT_ACTIVE",
            ));
        }
    }
    Ok(row)
}

fn overrides(row: &AttachmentRow) -> cf_object_storage::ResponseOverrides {
    let name = match row.profile() {
        SecurityProfile::PrivateE2ee => "attachment.bin".to_string(),
        SecurityProfile::Managed => row
            .safe_file_name
            .clone()
            .unwrap_or_else(|| "attachment".into()),
    };
    // Always a download (never rendered inline by a browser), with an ASCII fallback
    // name and the RFC 5987 UTF-8 name.
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded = cf_object_storage::uri_encode(&name, false);
    cf_object_storage::ResponseOverrides {
        content_type: Some(row.media_type.clone()),
        content_disposition: Some(format!(
            "attachment; filename=\"{}\"; filename*=UTF-8''{encoded}",
            ascii.replace('"', "_")
        )),
    }
}

/// `POST /v1/conversations/{id}/attachments/{attachmentId}/download-grants`.
pub async fn grant(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    attachment_id: Uuid,
) -> Result<Value> {
    require_storage(state)?;
    let row = authorize_read(state, ctx, acting, conversation_id, attachment_id).await?;
    let now = state.now();
    let signed = state
        .storage
        .signed_read_as(
            &row.object_key,
            state.attachments.grant_ttl,
            now,
            &overrides(&row),
        )
        .map_err(storage_error)?;
    crate::metrics::inc("messaging_attachment_grants_total", &[]);
    Ok(json!({
        "attachmentId": row.id,
        "method": "GET",
        "url": signed.url,
        "expiresAt": rfc3339(signed.expires_at),
        "size": row.declared_size,
        "sha256": row.declared_sha256,
        "cryptoFormat": row.crypto_format,
        "residualAccessSeconds": state.attachments.grant_ttl.as_secs(),
    }))
}

/// `GET /v1/conversations/{id}/attachments/{attachmentId}/content`: a proxied read that
/// re-checks authorization on every request (immediate revocation), bounded in size.
pub async fn proxied(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    attachment_id: Uuid,
) -> Result<(AttachmentRow, Vec<u8>)> {
    require_storage(state)?;
    let row = authorize_read(state, ctx, acting, conversation_id, attachment_id).await?;
    if row.declared_size as usize > state.attachments.proxy_max_bytes {
        return Err(Error::reason(
            ErrorCode::ContentRejected,
            "USE_DOWNLOAD_GRANT",
        ));
    }
    let bytes = state
        .storage
        .get(&row.object_key, state.attachments.proxy_max_bytes)
        .await
        .map_err(storage_error)?
        .ok_or(Error::reason(
            ErrorCode::AttachmentNotReady,
            "OBJECT_MISSING",
        ))?;
    if cf_object_storage::sha256_hex(&bytes) != row.declared_sha256 {
        tracing::error!(attachment = %row.id, "stored object no longer matches its digest");
        return Err(Error::code(ErrorCode::IntegrityCheckFailed));
    }
    Ok((row, bytes))
}

pub fn content_disposition(row: &AttachmentRow) -> String {
    overrides(row)
        .content_disposition
        .unwrap_or_else(|| "attachment".into())
}

// ---------------------------------------------------------------------------------------
// Messages referencing attachments
// ---------------------------------------------------------------------------------------

pub async fn message_attachment_ids<C: ConnectionTrait>(
    db: &C,
    message: Uuid,
) -> Result<Vec<Uuid>> {
    #[derive(FromQueryResult)]
    struct I {
        id: Uuid,
    }
    Ok(db::all::<I, _>(
        db,
        "SELECT id FROM messaging_attachments WHERE message_id = $1 ORDER BY created_at, id",
        vec![message.into()],
    )
    .await?
    .into_iter()
    .map(|i| i.id)
    .collect())
}

/// Validates the attachments a message references (before its transaction).
pub async fn check_message_attachments(
    state: &AppState,
    conversation: &ConversationRow,
    policy: &EffectivePolicy,
    sender: Uuid,
    ids: &[Uuid],
) -> Result<()> {
    if !attachments_enabled(policy, conversation) {
        return Err(Error::reason(
            ErrorCode::FeatureDisabled,
            "ATTACHMENTS_DISABLED",
        ));
    }
    let mut unique = ids.to_vec();
    unique.sort();
    unique.dedup();
    if unique.len() != ids.len() {
        return Err(Error::validation("attachmentIds must be distinct"));
    }
    if ids.len() as i64 > policy.attachments.max_per_message {
        return Err(Error::reason(
            ErrorCode::QuotaExceeded,
            "TOO_MANY_ATTACHMENTS",
        ));
    }
    let rows: Vec<AttachmentRow> = db::all(
        &state.db,
        &format!("SELECT {ATTACHMENT_COLUMNS} FROM messaging_attachments WHERE id = ANY($1) AND conversation_id = $2"),
        vec![ids.to_vec().into(), conversation.id.into()],
    )
    .await?;
    if rows.len() != ids.len() || rows.iter().any(|r| r.uploader_principal_id != sender) {
        return Err(Error::code(ErrorCode::NotFound));
    }
    if let Some(r) = rows.iter().find(|r| r.state != "READY") {
        return Err(Error::reason(
            ErrorCode::AttachmentNotReady,
            if r.state == "QUARANTINED" {
                "QUARANTINED"
            } else {
                "NOT_READY"
            },
        ));
    }
    if rows.iter().any(|r| r.message_id.is_some()) {
        return Err(Error::reason(
            ErrorCode::Conflict,
            "ATTACHMENT_ALREADY_USED",
        ));
    }
    Ok(())
}

/// Binds READY attachments to a new message (same transaction as the message).
pub async fn link_to_message<C: ConnectionTrait>(
    db: &C,
    conversation: &ConversationRow,
    message: Uuid,
    sender: Uuid,
    ids: &[Uuid],
) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let n = db::exec(
        db,
        "UPDATE messaging_attachments SET message_id = $1 WHERE id = ANY($2) AND conversation_id = $3 \
           AND uploader_principal_id = $4 AND state = 'READY' AND message_id IS NULL",
        vec![message.into(), ids.to_vec().into(), conversation.id.into(), sender.into()],
    )
    .await?;
    if n as usize != ids.len() {
        return Err(Error::reason(
            ErrorCode::Conflict,
            "ATTACHMENT_ALREADY_USED",
        ));
    }
    Ok(())
}

/// A tombstoned message's attachments stop being downloadable at once (the download
/// path checks the message lifecycle); their objects are removed by the purge.
pub async fn follow_message<C: ConnectionTrait>(
    _db: &C,
    _message: Uuid,
    _lifecycle: &str,
    _now: DateTime<Utc>,
) -> Result<()> {
    Ok(())
}

/// Request context helper for workers.
pub fn worker_context() -> crate::domain::RequestContext {
    request_context()
}
