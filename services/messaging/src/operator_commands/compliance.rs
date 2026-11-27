//! A5 legal holds and A7 deletion requests.
//!
//! | Command | Route | Risk | Status |
//! |---|---|---|---|
//! | `messaging.hold.place` | `POST /operator/holds` | R3 | `ACTIVE` (version 1) |
//! | `messaging.hold.release` | `POST /operator/holds/{holdId}/release` | R3 | `RELEASED` |
//! | `messaging.deletion-request.create` | `POST /operator/deletion-requests` | R4 | `PENDING` (no version) |
//! | `messaging.export.request` | `POST /operator/exports` | R4 | `REQUESTED` (no version; owner-delivered) |
//!
//! Both reuse the application administration's own paths ([`crate::compliance`]): a hold
//! takes the environment's exclusive hold lock (an in-flight purge finishes before the hold
//! is acknowledged) and preserves what is stored, never creating a way to decrypt; a
//! deletion request is only REGISTERED — the existing leased worker executes it later and
//! legal holds win (held items are preserved and counted, `PARTIALLY_HELD`). Nothing here
//! reads or returns content; the case reference of a hold is stored, never answered.
//! A created resource's id is the command's `Idempotency-Key`, so a replay names the same
//! subject.

use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::HeaderMap,
    Json,
};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    begin, body, conversation_scope, environment_scope, expected, finish, reason, version_of,
    BridgeCommand, Outcome, ScopeRow, Subject,
};
use crate::{
    compliance::{
        deletion::{self, DeletionRequest},
        exports,
        holds::{self, CreateHold},
    },
    conversations::service::rfc3339,
    db,
    error::{Error, ErrorCode},
    http::{error::ApiError, middleware::InternalCaller},
    state::AppState,
};

pub const PLACE_HOLD: &str = "messaging.hold.place";
pub const RELEASE_HOLD: &str = "messaging.hold.release";
pub const CREATE_DELETION_REQUEST: &str = "messaging.deletion-request.create";
pub const REQUEST_EXPORT: &str = "messaging.export.request";

/// The scope a tenant's command names: a conversation's (it must lie in `environment_id`)
/// or a known environment's.
async fn scope_for<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    environment_id: Uuid,
    conversation_id: Option<Uuid>,
) -> Result<ScopeRow, ApiError> {
    match conversation_id {
        Some(c) => {
            let scope = conversation_scope(db, c, tenant_id, false).await?;
            if scope.environment_id != environment_id {
                return Err(Error::code(ErrorCode::NotFound));
            }
            Ok(scope)
        }
        None => environment_scope(db, environment_id, tenant_id).await,
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlaceHold {
    pub tenant_id: Uuid,
    pub environment_id: Uuid,
    /// CONVERSATION (needs conversationId), PRINCIPAL (needs principalId) or ENVIRONMENT.
    pub scope_kind: String,
    #[serde(default)]
    pub conversation_id: Option<Uuid>,
    #[serde(default)]
    pub principal_id: Option<Uuid>,
    /// LITIGATION, REGULATORY, INVESTIGATION or INTERNAL_POLICY.
    pub reason_category: String,
    /// Stored for the record, never returned (1-128 of `[A-Za-z0-9._:/-]`).
    #[serde(default)]
    pub case_reference: Option<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

/// `POST /operator/holds` — `messaging.hold.place`.
pub async fn place_hold(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    raw: Result<Json<PlaceHold>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let cmd = BridgeCommand::of(&caller, &headers, true)?;
    let req = body(raw)?;
    let hold_id = cmd.key;
    let subject = Subject::new("LEGAL_HOLD", hold_id);
    let now = state.now();
    let txn = state.db.begin().await?;
    if let Some(done) = begin(&txn, &cmd, PLACE_HOLD, &subject).await? {
        return Ok(Json(done));
    }
    let scope = scope_for(&txn, req.tenant_id, req.environment_id, req.conversation_id).await?;
    let ctx = scope.context();
    let create = CreateHold {
        scope_kind: req.scope_kind.clone(),
        conversation_id: req.conversation_id,
        principal_id: req.principal_id,
        reason_category: req.reason_category.clone(),
        case_reference: req.case_reference.clone(),
        expires_at: req.expires_at,
    };
    holds::check_create(&txn, &ctx, &create, now).await?;
    holds::create_in(&txn, &ctx, hold_id, &create, now).await?;
    holds::audit_created(&txn, &ctx, hold_id, &create, now).await?;
    let outcome = Outcome {
        state: "ACTIVE",
        version: Some(1),
        resource: json!({
            "holdId": hold_id,
            "tenantId": scope.tenant_id,
            "applicationId": scope.application_id,
            "environmentId": scope.environment_id,
            "scopeKind": req.scope_kind,
            "conversationId": req.conversation_id,
            "principalId": req.principal_id,
            "reasonCategory": req.reason_category,
            "holdStatus": "ACTIVE",
            "createdAt": rfc3339(now),
            "expiresAt": req.expires_at.map(rfc3339),
        }),
    };
    let answer = finish(&txn, &cmd, PLACE_HOLD, &subject, outcome, now).await?;
    txn.commit().await?;
    crate::metrics::inc("messaging_legal_holds_created_total", &[]);
    state.wake_outbox();
    Ok(Json(answer))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseHold {
    pub tenant_id: Uuid,
    pub expected_version: i64,
    pub reason_code: String,
}

#[derive(Debug, FromQueryResult)]
struct HoldLock {
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    status: String,
    version: i64,
}

/// `POST /operator/holds/{holdId}/release` — `messaging.hold.release`. Version: the hold's.
pub async fn release_hold(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(hold_id): Path<Uuid>,
    raw: Result<Json<ReleaseHold>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let cmd = BridgeCommand::of(&caller, &headers, true)?;
    let req = body(raw)?;
    let version = expected(req.expected_version)?;
    reason(&req.reason_code)?;
    let subject = Subject::new("LEGAL_HOLD", hold_id);
    let now = state.now();
    let txn = state.db.begin().await?;
    if let Some(done) = begin(&txn, &cmd, RELEASE_HOLD, &subject).await? {
        return Ok(Json(done));
    }
    let hold: HoldLock = db::one(
        &txn,
        "SELECT tenant_id, application_id, environment_id, status, version FROM messaging_legal_holds \
         WHERE id = $1 FOR UPDATE",
        vec![hold_id.into()],
    )
    .await?
    .filter(|h: &HoldLock| h.tenant_id == req.tenant_id)
    .ok_or(Error::code(ErrorCode::NotFound))?;
    if hold.version != version {
        return Err(Error::code(ErrorCode::VersionConflict));
    }
    if hold.status != "ACTIVE" {
        return Err(Error::reason(ErrorCode::Conflict, "HOLD_NOT_ACTIVE"));
    }
    let ctx = ScopeRow {
        tenant_id: hold.tenant_id,
        application_id: hold.application_id,
        environment_id: hold.environment_id,
    }
    .context();
    if !holds::release_in(&txn, &ctx, hold_id, &req.reason_code, now).await? {
        return Err(Error::reason(ErrorCode::Conflict, "HOLD_NOT_ACTIVE"));
    }
    holds::audit_released(&txn, &ctx, hold_id, &req.reason_code, now).await?;
    let outcome = Outcome {
        state: "RELEASED",
        version: Some(
            version_of(
                &txn,
                "SELECT version FROM messaging_legal_holds WHERE id = $1",
                vec![hold_id.into()],
            )
            .await?,
        ),
        resource: json!({
            "holdId": hold_id,
            "holdStatus": "RELEASED",
            "releaseReason": req.reason_code,
            "releasedAt": rfc3339(now),
        }),
    };
    let answer = finish(&txn, &cmd, RELEASE_HOLD, &subject, outcome, now).await?;
    txn.commit().await?;
    state.wake_outbox();
    Ok(Json(answer))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateDeletionRequest {
    pub tenant_id: Uuid,
    pub environment_id: Uuid,
    /// PRINCIPAL_ERASURE (needs principalId) or CONVERSATION_PURGE (needs conversationId).
    pub kind: String,
    #[serde(default)]
    pub principal_id: Option<Uuid>,
    #[serde(default)]
    pub conversation_id: Option<Uuid>,
    pub reason_code: String,
}

/// `POST /operator/deletion-requests` — `messaging.deletion-request.create` (R4): registers
/// the request; the worker executes it later and legal holds win.
pub async fn create_deletion_request(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    raw: Result<Json<CreateDeletionRequest>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let cmd = BridgeCommand::of(&caller, &headers, true)?;
    let req = body(raw)?;
    reason(&req.reason_code)?;
    let request_id = cmd.key;
    let subject = Subject::new("DELETION_REQUEST", request_id);
    let now = state.now();
    let txn = state.db.begin().await?;
    if let Some(done) = begin(&txn, &cmd, CREATE_DELETION_REQUEST, &subject).await? {
        return Ok(Json(done));
    }
    let scope = scope_for(&txn, req.tenant_id, req.environment_id, req.conversation_id).await?;
    let ctx = scope.context();
    let request = DeletionRequest {
        kind: req.kind.clone(),
        principal_id: req.principal_id,
        conversation_id: req.conversation_id,
    };
    deletion::check(&txn, &ctx, &request).await?;
    deletion::create_in(&txn, &ctx, request_id, &request, now).await?;
    let outcome = Outcome {
        state: "PENDING",
        version: None,
        resource: json!({
            "requestId": request_id,
            "tenantId": scope.tenant_id,
            "applicationId": scope.application_id,
            "environmentId": scope.environment_id,
            "kind": req.kind,
            "principalId": req.principal_id,
            "conversationId": req.conversation_id,
            "requestStatus": "PENDING",
            "createdAt": rfc3339(now),
        }),
    };
    let answer = finish(&txn, &cmd, CREATE_DELETION_REQUEST, &subject, outcome, now).await?;
    txn.commit().await?;
    Ok(Json(answer))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestExport {
    pub tenant_id: Uuid,
    /// The MANAGED conversation to export.
    pub conversation_id: Uuid,
    /// The verified requester (the data subject): an ACTIVE member of the conversation who
    /// is a CF Meet account (`NATIVE_CF_USER`). The export is delivered to them only.
    pub recipient_principal_id: Uuid,
    pub reason_code: String,
}

/// `POST /operator/exports` — `messaging.export.request` (R4, two-person): an
/// OWNER-DELIVERED export (brief §1). Built by the existing export worker (MANAGED only;
/// a PRIVATE_E2EE conversation is refused with `E2EE_EXPORT_NOT_AVAILABLE`), encrypted at
/// rest, kept 7 days, and delivered only to the named recipient, who claims it once in
/// their own CF Meet console. The answer is a reference (ids, state, delivery), never the
/// export, a URL or an address. Legal holds never block an export (it reads, never
/// deletes) and are unaffected by it: the export copy's own expiry deletes only the copy.
pub async fn request_export(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    raw: Result<Json<RequestExport>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let cmd = BridgeCommand::of(&caller, &headers, true)?;
    let req = body(raw)?;
    reason(&req.reason_code)?;
    let export_id = cmd.key;
    let subject = Subject::new("EXPORT", export_id);
    // A replay answers before the recipient is looked up again.
    {
        let txn = state.db.begin().await?;
        if let Some(done) = begin(&txn, &cmd, REQUEST_EXPORT, &subject).await? {
            return Ok(Json(done));
        }
    }
    let scope = conversation_scope(&state.db, req.conversation_id, req.tenant_id, false).await?;
    let ctx = scope.context();
    verified_recipient(&state, &ctx, req.recipient_principal_id).await?;
    let now = state.now();
    let txn = state.db.begin().await?;
    if let Some(done) = begin(&txn, &cmd, REQUEST_EXPORT, &subject).await? {
        return Ok(Json(done));
    }
    exports::check_exportable(&state, &txn, &ctx, req.conversation_id)
        .await
        .map_err(super::definite)?;
    let member: Option<MemberState> = db::one(
        &txn,
        "SELECT state FROM messaging_conversation_members WHERE conversation_id = $1 AND principal_id = $2",
        vec![req.conversation_id.into(), req.recipient_principal_id.into()],
    )
    .await?;
    if member.is_none_or(|m| m.state != "ACTIVE") {
        return Err(Error::reason(ErrorCode::Conflict, "RECIPIENT_NOT_A_MEMBER"));
    }
    let row = exports::insert_owner_delivered(
        &txn,
        &ctx,
        export_id,
        req.conversation_id,
        req.recipient_principal_id,
        now,
    )
    .await?;
    let mut resource = exports::delivery_view(&row);
    resource["tenantId"] = json!(scope.tenant_id);
    resource["applicationId"] = json!(scope.application_id);
    resource["environmentId"] = json!(scope.environment_id);
    let outcome = Outcome {
        state: "REQUESTED",
        version: None,
        resource,
    };
    let answer = finish(&txn, &cmd, REQUEST_EXPORT, &subject, outcome, now).await?;
    txn.commit().await?;
    crate::metrics::inc("messaging_exports_total", &[("outcome", "owner_requested")]);
    Ok(Json(answer))
}

#[derive(Debug, FromQueryResult)]
struct MemberState {
    state: String,
}

/// The recipient must be an ACTIVE CF Meet account principal (`NATIVE_CF_USER` with a
/// `cfmeet-user:<account>` reference) of the conversation's scope: an account whose identity
/// CF Meet verified at sign-in. 409 `RECIPIENT_NOT_VERIFIED` otherwise; 503 when the
/// directory is unavailable (nothing done).
async fn verified_recipient(
    state: &AppState,
    ctx: &crate::identity::TrustedContext,
    principal: Uuid,
) -> Result<(), ApiError> {
    let refs = state
        .principals
        .native_user_references(&ctx.scope, &[principal])
        .await
        .map_err(|_| Error::code(ErrorCode::ServiceUnavailable))?;
    if refs
        .get(&principal)
        .and_then(|r| crate::notifications::account_ref(r))
        .is_none()
    {
        return Err(Error::reason(ErrorCode::Conflict, "RECIPIENT_NOT_VERIFIED"));
    }
    Ok(())
}
