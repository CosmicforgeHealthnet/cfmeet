//! Sections 9–23 private owner interfaces of Messaging for the Communications Hub
//! (contract `cfmeet.messaging.communications-internal`, MS-1 … MS-7).
//!
//! Messaging stays authoritative: every answer is read from the canonical tables at
//! request time, and the two mutations (device revoke, push registration) run the
//! canonical customer code paths. Each route requires the Hub's signed delegated context
//! for the governed personal scope (`own`, 404 otherwise) and acts only for the
//! NATIVE_CF_USER that context names (403 for any other `principalId`). No message
//! content, ciphertext, keys or push handles are read into a response or a log.
use super::own;
use crate::{
    conversations::{
        access,
        service::{decode_cursor, encode_cursor, rfc3339, ConversationRow},
    },
    db, devices,
    error::{Error, ErrorCode},
    http::{
        error::ApiError,
        middleware::{verify_principal, InternalCaller},
    },
    identity::TrustedContext,
    realtime::push::{self, PushProvider},
    state::AppState,
};
use axum::{extract::State, Json};
use cf_service_auth::{DelegatedPrincipalKind, InternalPermission};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

type Reply = std::result::Result<Json<Value>, ApiError>;

/// At most this many ACTIVE member ids travel in an access view (MS-1).
pub const ACCESS_MEMBER_IDS: i64 = 256;
/// At most this many devices are reported by the push status (MS-7).
pub const STATUS_DEVICES: i64 = 100;
/// The push services reported by MS-7 (`LOCAL_TEST` is never a production service).
pub const PUSH_SERVICES: [&str; 3] = ["APNS", "FCM", "WEB_PUSH"];

/// Whether the signed initiating principal is the NATIVE_CF_USER the body names.
pub fn initiating_matches(
    initiating: Option<(Uuid, DelegatedPrincipalKind)>,
    claimed: Uuid,
) -> bool {
    matches!(initiating, Some((id, DelegatedPrincipalKind::NativeCfUser)) if id == claimed)
}

/// `own` (permission, delegated first-party governed scope) and then the principal: the
/// Hub never acts for a user other than the one its signed context authenticates.
async fn own_for(
    state: &AppState,
    caller: &InternalCaller,
    permission: InternalPermission,
    principal: Uuid,
) -> std::result::Result<TrustedContext, ApiError> {
    let ctx = own(state, caller, permission).await?;
    let initiating = caller
        .context()
        .delegated_platform_context()
        .and_then(|d| d.initiating_principal())
        .map(|p| (p.principal_id, p.principal_kind));
    if !initiating_matches(initiating, principal) {
        return Err(ApiError::reason(
            ErrorCode::PrincipalNotAuthorized,
            "DELEGATED_PRINCIPAL_MISMATCH",
        ));
    }
    Ok(ctx)
}

/// MS-1 message state: tombstones by kind; a retention purge stays EXPIRED.
pub fn message_state(lifecycle: &str, reason: Option<&str>, hidden: bool) -> &'static str {
    match lifecycle {
        "ACTIVE" if hidden => "HIDDEN_FOR_ME",
        "ACTIVE" => "VISIBLE",
        "EXPIRED" => "EXPIRED",
        "PURGED" if reason == Some("RETENTION_EXPIRED") => "EXPIRED",
        _ => "DELETED",
    }
}

/// MS-2 media class. Anything that is not MANAGED is ENCRYPTED (fail closed): the
/// server never inspects or classifies PRIVATE_E2EE ciphertext.
pub fn media_class(security_profile: &str, kind: &str, media_type: &str) -> &'static str {
    if security_profile != "MANAGED" {
        return "ENCRYPTED";
    }
    match kind {
        "IMAGE" => "IMAGE",
        "VIDEO" => "VIDEO",
        "VOICE_NOTE" => "AUDIO",
        "DOCUMENT" => "DOCUMENT",
        _ => match media_type.split('/').next() {
            Some("image") => "IMAGE",
            Some("video") => "VIDEO",
            Some("audio") => "AUDIO",
            _ => "OTHER",
        },
    }
}

/// MS-7: what this deployment can actually deliver to (never inferred from registrations).
pub fn push_services(provider: &dyn PushProvider) -> Value {
    let mut out = serde_json::Map::new();
    for service in PUSH_SERVICES {
        out.insert(
            service.into(),
            json!(if provider.supports(service) {
                "CONFIGURED"
            } else {
                "NOT_CONFIGURED"
            }),
        );
    }
    Value::Object(out)
}

/// MS-7 failure class of a device's latest push job: coarse classes only, never the
/// provider's error text.
pub fn failure_class(
    status: &str,
    attempts: i32,
    errored: bool,
    invalid_handle: bool,
) -> Option<&'static str> {
    match status {
        "FAILED" if invalid_handle => Some("INVALID_HANDLE"),
        "FAILED" => Some("DELIVERY_FAILED"),
        "NOT_CONFIGURED" => Some("NOT_CONFIGURED"),
        "EXPIRED" => Some("EXPIRED"),
        "DISABLED" => Some("DISABLED"),
        "PENDING" if attempts > 0 && errored => Some("RETRYING"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------
// MS-1 conversation access
// ---------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessRequest {
    principal_id: Uuid,
    conversation_id: Uuid,
    #[serde(default)]
    message_id: Option<Uuid>,
}

#[derive(FromQueryResult)]
struct MessageMeta {
    id: Uuid,
    sequence: i64,
    message_kind: String,
    sender_principal_id: Uuid,
    lifecycle: String,
    lifecycle_reason: Option<String>,
    created_at: DateTime<Utc>,
}

#[derive(FromQueryResult)]
struct PrincipalId {
    principal_id: Uuid,
}

async fn message_view<C: ConnectionTrait>(
    db: &C,
    conversation: &ConversationRow,
    member: &access::Membership,
    principal: Uuid,
    message_id: Uuid,
) -> crate::error::Result<Value> {
    // Metadata columns only: never the ciphertext or the managed sealed text.
    let row: Option<MessageMeta> = db::one(
        db,
        "SELECT id, sequence, message_kind, sender_principal_id, lifecycle, lifecycle_reason, created_at \
           FROM messaging_messages WHERE conversation_id = $1 AND id = $2",
        vec![conversation.id.into(), message_id.into()],
    )
    .await?;
    // Unknown, not an application message, or before the member's history window: the
    // same answer (permalink rule, §84).
    let Some(row) = row
        .filter(|r| r.message_kind == "APPLICATION" && r.sequence >= member.visible_from_sequence)
    else {
        return Ok(json!({
            "messageId": message_id,
            "sequence": null,
            "state": "NOT_FOUND",
            "sentAt": null,
            "senderPrincipalId": null,
        }));
    };
    let hidden = !crate::messages::hidden_for(db, principal, &[row.id])
        .await?
        .is_empty();
    Ok(json!({
        "messageId": row.id,
        "sequence": row.sequence,
        "state": message_state(&row.lifecycle, row.lifecycle_reason.as_deref(), hidden),
        "sentAt": rfc3339(row.created_at),
        "senderPrincipalId": row.sender_principal_id,
    }))
}

pub async fn conversation_access(
    State(s): State<AppState>,
    caller: InternalCaller,
    Json(p): Json<AccessRequest>,
) -> Reply {
    let ctx = own_for(
        &s,
        &caller,
        InternalPermission::MessagingCommunicationsRead,
        p.principal_id,
    )
    .await?;
    let not_member = || Ok(Json(json!({"schemaVersion": 1, "member": false})));
    let Some(c) =
        crate::conversations::service::load(&s.db, &ctx, p.conversation_id, false).await?
    else {
        return not_member();
    };
    let member = match access::membership(&s.db, c.id, p.principal_id, false).await? {
        Some(m) if m.is_active() => m,
        _ => return not_member(),
    };
    let last_sequence = db::scalar_i64(
        &s.db,
        "SELECT COALESCE((SELECT next_sequence - 1 FROM messaging_conversation_sequences WHERE conversation_id = $1), 0)",
        vec![c.id.into()],
    )
    .await?;
    let ids: Vec<PrincipalId> = db::all(
        &s.db,
        "SELECT principal_id FROM messaging_conversation_members WHERE conversation_id = $1 AND state = 'ACTIVE' \
         ORDER BY principal_id LIMIT $2",
        vec![c.id.into(), ACCESS_MEMBER_IDS.into()],
    )
    .await?;
    let member_count = db::scalar_i64(
        &s.db,
        "SELECT count(*) FROM messaging_conversation_members WHERE conversation_id = $1 AND state = 'ACTIVE'",
        vec![c.id.into()],
    )
    .await?;
    let mut v = json!({
        "schemaVersion": 1,
        "member": true,
        "conversationId": c.id,
        "contextClass": c.context_class,
        "securityProfile": c.security_profile,
        "lifecycleState": c.lifecycle_state,
        "lastSequence": last_sequence,
        "visibleFromSequence": member.visible_from_sequence,
        "retentionSeconds": c.retention_seconds,
        "expiresAt": c.expires_at.map(rfc3339),
        "role": member.role,
        "memberCount": member_count,
        "memberPrincipalIds": ids.iter().map(|m| m.principal_id).collect::<Vec<_>>(),
    });
    if let Some(mid) = p.message_id {
        v["message"] = message_view(&s.db, &c, &member, p.principal_id, mid).await?;
    }
    Ok(Json(v))
}

// ---------------------------------------------------------------------------------------
// MS-2 conversation attachments
// ---------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachmentsRequest {
    principal_id: Uuid,
    conversation_id: Uuid,
    limit: u64,
    #[serde(default)]
    cursor: Option<String>,
    /// Excludes attachments whose message sequence is `<= afterSequence` (the Hub's
    /// clear-for-me watermark).
    #[serde(default)]
    after_sequence: i64,
}

#[derive(FromQueryResult)]
struct AttachmentMeta {
    id: Uuid,
    message_id: Option<Uuid>,
    message_sequence: Option<i64>,
    state: String,
    size_bytes: i64,
    security_profile: String,
    kind: String,
    media_type: String,
    created_at: DateTime<Utc>,
    uploader_principal_id: Uuid,
}

pub async fn conversation_attachments(
    State(s): State<AppState>,
    caller: InternalCaller,
    Json(p): Json<AttachmentsRequest>,
) -> Reply {
    let ctx = own_for(
        &s,
        &caller,
        InternalPermission::MessagingCommunicationsRead,
        p.principal_id,
    )
    .await?;
    if !(1..=100).contains(&p.limit) {
        return Err(Error::validation("limit must be between 1 and 100"));
    }
    if p.after_sequence < 0 {
        return Err(Error::validation("afterSequence must not be negative"));
    }
    let cursor = match p.cursor.as_deref() {
        None => None,
        Some(c) => Some(decode_cursor(c).ok_or_else(|| Error::validation("invalid cursor"))?),
    };
    let (cursor_t, cursor_id) = cursor
        .map(|(t, i)| (Some(t), Some(i)))
        .unwrap_or((None, None));
    // Canonical membership: a non-member is told NOT_FOUND, as on the customer surface.
    let (c, member) = access::as_member(&s.db, &ctx, p.conversation_id, p.principal_id).await?;
    // Message-attached attachments of ACTIVE application messages inside the member's
    // history window, not hidden for this principal. Metadata only.
    let rows: Vec<AttachmentMeta> = db::all(
        &s.db,
        "SELECT a.id, a.message_id, m.sequence AS message_sequence, a.state, \
                COALESCE(a.verified_size, a.declared_size) AS size_bytes, a.security_profile, a.kind, \
                a.media_type, a.created_at, a.uploader_principal_id \
           FROM messaging_attachments a \
           JOIN messaging_messages m ON m.id = a.message_id AND m.conversation_id = a.conversation_id \
          WHERE a.conversation_id = $1 AND a.tenant_id = $2 AND a.application_id = $3 AND a.environment_id = $4 \
            AND m.message_kind = 'APPLICATION' AND m.lifecycle = 'ACTIVE' \
            AND m.sequence >= $5 AND m.sequence > $6 \
            AND NOT EXISTS (SELECT 1 FROM messaging_message_hidden h WHERE h.message_id = m.id AND h.principal_id = $7) \
            AND ($8::timestamptz IS NULL OR (a.created_at, a.id) < ($8, $9)) \
          ORDER BY a.created_at DESC, a.id DESC LIMIT $10",
        vec![
            c.id.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            member.visible_from_sequence.into(),
            p.after_sequence.into(),
            p.principal_id.into(),
            cursor_t.into(),
            cursor_id.into(),
            ((p.limit + 1) as i64).into(),
        ],
    )
    .await?;
    let more = rows.len() as u64 > p.limit;
    let page: Vec<_> = rows.into_iter().take(p.limit as usize).collect();
    let next = if more {
        page.last().map(|r| encode_cursor(r.created_at, r.id))
    } else {
        None
    };
    Ok(Json(json!({
        "schemaVersion": 1,
        "data": page.iter().map(|a| json!({
            "attachmentId": a.id,
            "messageId": a.message_id,
            "messageSequence": a.message_sequence,
            "state": a.state,
            "sizeBytes": a.size_bytes,
            "mediaClass": media_class(&a.security_profile, &a.kind, &a.media_type),
            "createdAt": rfc3339(a.created_at),
            "uploaderPrincipalId": a.uploader_principal_id,
        })).collect::<Vec<_>>(),
        "nextCursor": next,
    })))
}

// ---------------------------------------------------------------------------------------
// MS-3 storage summary
// ---------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrincipalRequest {
    principal_id: Uuid,
}

#[derive(FromQueryResult)]
struct Totals {
    n: i64,
    bytes: i64,
}

pub async fn storage_summary(
    State(s): State<AppState>,
    caller: InternalCaller,
    Json(p): Json<PrincipalRequest>,
) -> Reply {
    let ctx = own_for(
        &s,
        &caller,
        InternalPermission::MessagingCommunicationsRead,
        p.principal_id,
    )
    .await?;
    let scope = || -> Vec<sea_orm::Value> {
        vec![
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            p.principal_id.into(),
        ]
    };
    // Bytes the quota accounting holds committed for this uploader's attachments.
    let attachments: Totals = db::one(
        &s.db,
        "SELECT count(*)::bigint AS n, COALESCE(sum(committed_bytes), 0)::bigint AS bytes FROM messaging_attachments \
          WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND uploader_principal_id = $4 \
            AND committed_bytes > 0",
        scope(),
    )
    .await?
    .ok_or(Error::code(ErrorCode::InternalError))?;
    let backups: Totals = db::one(
        &s.db,
        "SELECT count(*)::bigint AS n, COALESCE(sum(size), 0)::bigint AS bytes FROM messaging_backups \
          WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND principal_id = $4 AND status = 'READY'",
        scope(),
    )
    .await?
    .ok_or(Error::code(ErrorCode::InternalError))?;
    let policy = crate::conversations::policy::current(&s.db, ctx.scope.environment_id).await?;
    let quota = crate::attachments::quota::effective_budget(
        &policy.attachments,
        s.attachments.storage_budget_bytes,
    );
    Ok(Json(json!({
        "schemaVersion": 1,
        "attachmentCount": attachments.n,
        "attachmentBytes": attachments.bytes,
        "backupBytes": backups.bytes,
        "environmentQuotaBytes": quota,
        "measuredAt": rfc3339(s.now()),
    })))
}

// ---------------------------------------------------------------------------------------
// MS-4 / MS-5 / MS-6 devices
// ---------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceRequest {
    principal_id: Uuid,
    device_id: Uuid,
}

#[derive(FromQueryResult)]
struct DevicePushRow {
    status: String,
    platform: Option<String>,
    enabled: Option<bool>,
}

/// The principal's own device in scope with its push registration (handle never read).
async fn own_device_push<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    principal: Uuid,
    device: Uuid,
) -> crate::error::Result<Option<DevicePushRow>> {
    Ok(db::one(
        db,
        "SELECT d.status, r.platform, r.enabled FROM messaging_devices d \
           LEFT JOIN messaging_push_registrations r ON r.device_id = d.id \
          WHERE d.id = $1 AND d.tenant_id = $2 AND d.application_id = $3 AND d.environment_id = $4 AND d.principal_id = $5",
        vec![
            device.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            principal.into(),
        ],
    )
    .await?)
}

fn registered(row: &DevicePushRow) -> (bool, Option<&str>) {
    let on = row.enabled == Some(true);
    (on, if on { row.platform.as_deref() } else { None })
}

pub async fn device_verify(
    State(s): State<AppState>,
    caller: InternalCaller,
    Json(p): Json<DeviceRequest>,
) -> Reply {
    let ctx = own_for(
        &s,
        &caller,
        InternalPermission::MessagingCommunicationsRead,
        p.principal_id,
    )
    .await?;
    Ok(Json(
        match own_device_push(&s.db, &ctx, p.principal_id, p.device_id).await? {
            None => json!({
                "schemaVersion": 1, "owned": false, "status": null, "pushRegistered": false, "pushPlatform": null,
            }),
            Some(row) => {
                let (on, platform) = registered(&row);
                json!({
                    "schemaVersion": 1, "owned": true, "status": row.status, "pushRegistered": on, "pushPlatform": platform,
                })
            }
        },
    ))
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RevokeReason {
    UserRequested,
    Compromised,
}

impl RevokeReason {
    pub fn as_str(self) -> &'static str {
        match self {
            RevokeReason::UserRequested => "USER_REQUESTED",
            RevokeReason::Compromised => "COMPROMISED",
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RevokeDeviceRequest {
    principal_id: Uuid,
    device_id: Uuid,
    reason: RevokeReason,
}

pub async fn device_revoke(
    State(s): State<AppState>,
    caller: InternalCaller,
    Json(p): Json<RevokeDeviceRequest>,
) -> Reply {
    let ctx = own_for(
        &s,
        &caller,
        InternalPermission::MessagingCommunicationsApply,
        p.principal_id,
    )
    .await?;
    // The canonical path acts for a verified principal (Platform Access, fresh).
    let acting = verify_principal(&s, &ctx, p.principal_id).await?;
    let before = devices::load_device(&s.db, &ctx, p.device_id, false)
        .await?
        .filter(|d| d.principal_id == acting.principal_id)
        .ok_or(ApiError::code(ErrorCode::NotFound))?;
    if before.status == "REVOKED" {
        return Ok(Json(
            json!({"schemaVersion": 1, "revoked": true, "alreadyRevoked": true}),
        ));
    }
    let device = devices::revoke(
        &s,
        &ctx,
        &acting,
        p.device_id,
        devices::RevokeRequest {
            reason: p.reason.as_str().to_owned(),
        },
    )
    .await?;
    s.wake_outbox();
    Ok(Json(json!({
        "schemaVersion": 1,
        "revoked": device.status == "REVOKED",
        "alreadyRevoked": false,
    })))
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PushAction {
    Register,
    Remove,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DevicePushRequest {
    principal_id: Uuid,
    device_id: Uuid,
    action: PushAction,
    /// Exactly the customer push-registration body; passed through, never echoed.
    #[serde(default)]
    registration: Option<push::RegistrationRequest>,
}

pub async fn device_push(
    State(s): State<AppState>,
    caller: InternalCaller,
    Json(p): Json<DevicePushRequest>,
) -> Reply {
    let ctx = own_for(
        &s,
        &caller,
        InternalPermission::MessagingCommunicationsApply,
        p.principal_id,
    )
    .await?;
    let acting = verify_principal(&s, &ctx, p.principal_id).await?;
    match (p.action, p.registration) {
        (PushAction::Register, Some(registration)) => {
            let v = push::register(&s, &ctx, &acting, p.device_id, registration).await?;
            Ok(Json(json!({
                "schemaVersion": 1,
                "deviceId": p.device_id,
                "pushRegistered": v["enabled"] == true,
                "platform": if v["enabled"] == true { v["platform"].clone() } else { Value::Null },
            })))
        }
        (PushAction::Remove, None) => {
            let row = own_device_push(&s.db, &ctx, acting.principal_id, p.device_id)
                .await?
                .ok_or(ApiError::code(ErrorCode::NotFound))?;
            // Removing what is not registered is already the requested state.
            if registered(&row).0 {
                push::unregister(&s, &ctx, &acting, p.device_id).await?;
            }
            Ok(Json(json!({
                "schemaVersion": 1, "deviceId": p.device_id, "pushRegistered": false, "platform": null,
            })))
        }
        (PushAction::Register, None) => {
            Err(Error::validation("registration is required for REGISTER"))
        }
        (PushAction::Remove, Some(_)) => {
            Err(Error::validation("registration is not allowed for REMOVE"))
        }
    }
}

// ---------------------------------------------------------------------------------------
// MS-7 push status
// ---------------------------------------------------------------------------------------

#[derive(FromQueryResult)]
struct DeviceJobRow {
    device_id: Uuid,
    platform: Option<String>,
    enabled: Option<bool>,
    last_status: Option<String>,
    last_attempts: Option<i32>,
    last_errored: Option<bool>,
    last_invalid_handle: Option<bool>,
    last_completed_at: Option<DateTime<Utc>>,
}

#[derive(FromQueryResult)]
struct JobCounts {
    pending: i64,
    dead: i64,
}

pub async fn push_status(
    State(s): State<AppState>,
    caller: InternalCaller,
    Json(p): Json<PrincipalRequest>,
) -> Reply {
    let ctx = own_for(
        &s,
        &caller,
        InternalPermission::MessagingCommunicationsRead,
        p.principal_id,
    )
    .await?;
    let scope = || -> Vec<sea_orm::Value> {
        vec![
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            p.principal_id.into(),
        ]
    };
    // The handle and the provider's error text are never selected.
    let mut values = scope();
    values.push(STATUS_DEVICES.into());
    let rows: Vec<DeviceJobRow> = db::all(
        &s.db,
        "SELECT d.id AS device_id, r.platform, r.enabled, j.status AS last_status, j.attempts AS last_attempts, \
                (j.last_error IS NOT NULL) AS last_errored, (j.last_error = 'invalid handle') AS last_invalid_handle, \
                j.completed_at AS last_completed_at \
           FROM messaging_devices d \
           LEFT JOIN messaging_push_registrations r ON r.device_id = d.id \
           LEFT JOIN LATERAL (SELECT status, attempts, last_error, completed_at FROM messaging_push_jobs \
                               WHERE device_id = d.id ORDER BY created_at DESC, id DESC LIMIT 1) j ON TRUE \
          WHERE d.tenant_id = $1 AND d.application_id = $2 AND d.environment_id = $3 AND d.principal_id = $4 \
            AND d.status = 'ACTIVE' \
          ORDER BY d.registered_at, d.id LIMIT $5",
        values,
    )
    .await?;
    let counts: JobCounts = db::one(
        &s.db,
        "SELECT count(*) FILTER (WHERE j.status = 'PENDING')::bigint AS pending, \
                count(*) FILTER (WHERE j.status = 'FAILED')::bigint AS dead \
           FROM messaging_push_jobs j JOIN messaging_devices d ON d.id = j.device_id \
          WHERE d.tenant_id = $1 AND d.application_id = $2 AND d.environment_id = $3 AND d.principal_id = $4 \
            AND j.status IN ('PENDING', 'FAILED')",
        scope(),
    )
    .await?
    .ok_or(Error::code(ErrorCode::InternalError))?;
    let devices: Vec<Value> = rows
        .iter()
        .map(|r| {
            let on = r.enabled == Some(true);
            json!({
                "deviceId": r.device_id,
                "pushRegistered": on,
                "platform": if on { r.platform.as_deref() } else { None },
                "lastJobState": r.last_status,
                "lastFailureClass": r.last_status.as_deref().and_then(|st| failure_class(
                    st,
                    r.last_attempts.unwrap_or(0),
                    r.last_errored == Some(true),
                    r.last_invalid_handle == Some(true),
                )),
                "lastAttemptAt": r.last_completed_at.map(rfc3339),
            })
        })
        .collect();
    Ok(Json(json!({
        "schemaVersion": 1,
        "pushServices": push_services(s.realtime.push.as_ref()),
        "devices": devices,
        "pendingJobs": counts.pending,
        "deadJobs": counts.dead,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_signed_native_user_may_be_named() {
        let me = Uuid::new_v4();
        assert!(initiating_matches(
            Some((me, DelegatedPrincipalKind::NativeCfUser)),
            me
        ));
        assert!(!initiating_matches(
            Some((me, DelegatedPrincipalKind::NativeCfUser)),
            Uuid::new_v4()
        ));
        assert!(!initiating_matches(
            Some((me, DelegatedPrincipalKind::ServiceAccount)),
            me
        ));
        assert!(!initiating_matches(None, me));
    }

    #[test]
    fn e2ee_attachments_are_always_encrypted() {
        for kind in ["IMAGE", "VIDEO", "VOICE_NOTE", "DOCUMENT", "FILE"] {
            assert_eq!(media_class("PRIVATE_E2EE", kind, "image/png"), "ENCRYPTED");
            assert_eq!(media_class("UNKNOWN", kind, "image/png"), "ENCRYPTED");
        }
        assert_eq!(media_class("MANAGED", "IMAGE", "image/png"), "IMAGE");
        assert_eq!(media_class("MANAGED", "VOICE_NOTE", "audio/ogg"), "AUDIO");
        assert_eq!(media_class("MANAGED", "FILE", "audio/mpeg"), "AUDIO");
        assert_eq!(media_class("MANAGED", "FILE", "application/zip"), "OTHER");
        assert_eq!(
            media_class("MANAGED", "DOCUMENT", "application/pdf"),
            "DOCUMENT"
        );
    }

    #[test]
    fn message_states_follow_the_canonical_lifecycle() {
        assert_eq!(message_state("ACTIVE", None, false), "VISIBLE");
        assert_eq!(message_state("ACTIVE", None, true), "HIDDEN_FOR_ME");
        assert_eq!(
            message_state("DELETED", Some("SENDER_DELETED"), true),
            "DELETED"
        );
        assert_eq!(message_state("MODERATED", None, false), "DELETED");
        assert_eq!(
            message_state("EXPIRED", Some("RETENTION_EXPIRED"), false),
            "EXPIRED"
        );
        assert_eq!(
            message_state("PURGED", Some("RETENTION_EXPIRED"), false),
            "EXPIRED"
        );
        assert_eq!(message_state("PURGED", None, false), "DELETED");
    }

    #[test]
    fn push_services_report_only_real_configuration() {
        let none = push_services(&push::UnconfiguredPush);
        let local = push_services(&push::LocalTestPush::default());
        for v in [none, local] {
            for service in PUSH_SERVICES {
                assert_eq!(v[service], "NOT_CONFIGURED");
            }
            assert!(v.get("LOCAL_TEST").is_none());
        }
    }

    #[test]
    fn failure_classes_never_carry_provider_text() {
        assert_eq!(failure_class("SENT", 1, false, false), None);
        assert_eq!(failure_class("PENDING", 0, false, false), None);
        assert_eq!(failure_class("PENDING", 2, true, false), Some("RETRYING"));
        assert_eq!(
            failure_class("FAILED", 1, true, true),
            Some("INVALID_HANDLE")
        );
        assert_eq!(
            failure_class("FAILED", 8, true, false),
            Some("DELIVERY_FAILED")
        );
        assert_eq!(
            failure_class("NOT_CONFIGURED", 1, false, false),
            Some("NOT_CONFIGURED")
        );
    }
}
