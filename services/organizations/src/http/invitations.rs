//! Invitations. The raw token (256 random bits, base64url) is returned once at creation
//! and only its SHA-256 is stored; acceptance is single-use, expires after 7 days and is
//! bound to the invited email through `X-CF-Acting-User-Verified-Email` (supplied by the
//! account authority's relay). Re-inviting the same email replaces the pending invitation
//! (at most one PENDING per organization + email).

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use base64::Engine;
use cf_service_auth::InternalPermission;
use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use sea_orm::{ConnectionTrait, DatabaseTransaction, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{authorize, middleware::Caller, parse, respond};
use crate::{
    db,
    domain::{can_invite, Capability, LifecycleStatus, Role},
    error::{ApiError, ApiResult, ErrorCode},
    idempotency,
    state::AppState,
    store::{
        self, Event, InvitationRow, MembershipRow, OpContext, INVITATION_COLUMNS,
        MEMBERSHIP_COLUMNS,
    },
    validation::{email_hint, normalize_email},
};

pub const INVITATION_TTL_DAYS: i64 = 7;

/// A fresh one-time token and its stored digest.
pub fn new_token() -> (String, String) {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let hash = token_hash(&token);
    (token, hash)
}

pub fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn invalid() -> ApiError {
    ApiError::new(ErrorCode::InvitationInvalid)
}

async fn emit_invitation<C: ConnectionTrait>(
    c: &C,
    ctx: &OpContext,
    now: DateTime<Utc>,
    inv: &InvitationRow,
    event_type: &str,
    action: &str,
) -> Result<(), ApiError> {
    let mut payload = json!({ "invitationId": inv.id, "role": inv.role });
    if let Some(u) = inv.invitee_user_id {
        payload["inviteeUserId"] = json!(u);
    }
    store::emit(
        c,
        ctx,
        now,
        Event {
            organization_id: inv.organization_id,
            aggregate_type: "invitation",
            aggregate_id: inv.id,
            aggregate_version: inv.version,
            event_type,
            payload,
        },
    )
    .await?;
    store::audit(
        c,
        ctx,
        now,
        Some(inv.organization_id),
        action,
        "invitation",
        Some(inv.id),
        json!({ "role": inv.role, "inviteeUserId": inv.invitee_user_id }),
    )
    .await?;
    Ok(())
}

/// Moves a PENDING invitation to a terminal `status`.
async fn decide(
    txn: &DatabaseTransaction,
    ctx: &OpContext,
    now: DateTime<Utc>,
    inv: &InvitationRow,
    status: &str,
    invitee: Option<Uuid>,
) -> Result<InvitationRow, ApiError> {
    let row: InvitationRow = db::one(
        txn,
        &format!(
            "UPDATE organization_invitations SET status = $2, decided_at = $3, invitee_user_id = COALESCE($4, invitee_user_id), version = version + 1 WHERE id = $1 RETURNING {INVITATION_COLUMNS}"
        ),
        vec![inv.id.into(), status.into(), now.into(), invitee.into()],
    )
    .await?
    .ok_or_else(invalid)?;
    let event = format!("invitation.{}", status.to_ascii_lowercase());
    emit_invitation(txn, ctx, now, &row, &event, &event).await?;
    Ok(row)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateBody {
    pub email: String,
    pub role: String,
}

/// `POST /internal/v1/organizations/{id}/invitations` → the invitation and its one-time
/// `token`. An idempotent replay re-issues a fresh token for the same (still pending)
/// invitation, since the raw token is never stored.
pub async fn create(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationWrite)?;
    let user = caller.user()?;
    let (req, raw): (CreateBody, Value) = parse(&body)?;
    let email = normalize_email(&req.email)
        .ok_or_else(|| ApiError::validation("email must be an email address"))?;
    let role = Role::parse(&req.role)
        .ok_or_else(|| ApiError::validation("role must be ADMIN, MANAGER, MEMBER or GUEST"))?;
    let idem = idempotency::from_headers(
        &headers,
        format!("invitation.create:{}:{user}:{id}", caller.service()),
        &raw,
    )?;
    let txn = state.db.begin().await?;
    if let Some(i) = &idem {
        if let Some(stored) = idempotency::begin(&txn, i).await? {
            let mut doc = stored.response_body;
            if let Some(inv_id) = stored.resource_id {
                let (token, hash) = new_token();
                let rotated = db::exec(
                    &txn,
                    "UPDATE organization_invitations SET token_hash = $2 WHERE id = $1 AND status = 'PENDING' AND expires_at > now()",
                    vec![inv_id.into(), hash.into()],
                )
                .await?;
                if rotated == 1 {
                    doc["token"] = json!(token);
                }
            }
            txn.commit().await?;
            return Ok(respond(
                StatusCode::from_u16(stored.response_status as u16).unwrap_or(StatusCode::CREATED),
                doc,
            ));
        }
    }
    let access = authorize(&txn, id, user, Capability::MembersInvite, true).await?;
    if !can_invite(access.membership.role(), role) {
        return Err(ApiError::new(ErrorCode::PermissionDenied));
    }
    let now = db::now(&txn).await?;
    let ctx = caller.as_user();
    let existing: Option<InvitationRow> = db::one(
        &txn,
        &format!("SELECT {INVITATION_COLUMNS} FROM organization_invitations WHERE organization_id = $1 AND email_normalized = $2 AND status = 'PENDING' FOR UPDATE"),
        vec![id.into(), email.clone().into()],
    )
    .await?;
    if let Some(old) = existing {
        let status = if old.expires_at <= now {
            "EXPIRED"
        } else {
            "REVOKED"
        };
        decide(&txn, &ctx, now, &old, status, None).await?;
    }
    let (token, hash) = new_token();
    let inv: InvitationRow = db::one(
        &txn,
        &format!(
            "INSERT INTO organization_invitations (id, organization_id, token_hash, email_normalized, email_hint, role, status, invited_by, created_at, expires_at, version) \
             VALUES ($1, $2, $3, $4, $5, $6, 'PENDING', $7, $8, $9, 1) RETURNING {INVITATION_COLUMNS}"
        ),
        vec![
            Uuid::new_v4().into(),
            id.into(),
            hash.into(),
            email.clone().into(),
            email_hint(&email).into(),
            role.as_str().into(),
            user.into(),
            now.into(),
            (now + Duration::days(INVITATION_TTL_DAYS)).into(),
        ],
    )
    .await?
    .ok_or(ApiError::new(ErrorCode::ServiceUnavailable))?;
    emit_invitation(
        &txn,
        &ctx,
        now,
        &inv,
        "invitation.created",
        "invitation.created",
    )
    .await?;
    let doc = inv.to_json(now);
    if let Some(i) = &idem {
        idempotency::finish(&txn, i, 201, &doc, Some(inv.id), now).await?;
    }
    txn.commit().await?;
    let mut doc = doc;
    doc["token"] = json!(token);
    Ok(respond(StatusCode::CREATED, doc))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListQuery {
    #[serde(default)]
    pub status: Option<String>,
}

/// `GET /internal/v1/organizations/{id}/invitations?status` — newest first (200 max);
/// requires `organization.members.invite` for the actor.
pub async fn list(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationRead)?;
    let user = caller.user()?;
    if let Some(s) = &q.status {
        if !matches!(
            s.as_str(),
            "PENDING" | "ACCEPTED" | "DECLINED" | "REVOKED" | "EXPIRED"
        ) {
            return Err(ApiError::validation(
                "status must be PENDING, ACCEPTED, DECLINED, REVOKED or EXPIRED",
            ));
        }
    }
    authorize(&state.db, id, user, Capability::MembersInvite, false).await?;
    let now = db::now(&state.db).await?;
    let rows: Vec<InvitationRow> = db::all(
        &state.db,
        &format!("SELECT {INVITATION_COLUMNS} FROM organization_invitations WHERE organization_id = $1 ORDER BY created_at DESC, id DESC LIMIT 200"),
        vec![id.into()],
    )
    .await?;
    let items: Vec<Value> = rows
        .iter()
        .filter(|r| {
            q.status
                .as_deref()
                .is_none_or(|s| r.effective_status(now) == s)
        })
        .map(|r| r.to_json(now))
        .collect();
    Ok(respond(StatusCode::OK, json!({ "items": items })))
}

/// `POST /internal/v1/organizations/{id}/invitations/{invitationId}/revoke` — idempotent.
pub async fn revoke(
    State(state): State<AppState>,
    caller: Caller,
    Path((id, invitation_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationWrite)?;
    let user = caller.user()?;
    let txn = state.db.begin().await?;
    let access = authorize(&txn, id, user, Capability::MembersInvite, true).await?;
    let inv: InvitationRow = db::one(
        &txn,
        &format!("SELECT {INVITATION_COLUMNS} FROM organization_invitations WHERE organization_id = $1 AND id = $2 FOR UPDATE"),
        vec![id.into(), invitation_id.into()],
    )
    .await?
    .ok_or_else(invalid)?;
    let role = Role::parse(&inv.role).unwrap_or(Role::Admin);
    if !can_invite(access.membership.role(), role) {
        return Err(ApiError::new(ErrorCode::PermissionDenied));
    }
    let now = db::now(&txn).await?;
    let doc = match inv.status.as_str() {
        "REVOKED" => inv.to_json(now),
        "PENDING" => decide(&txn, &caller.as_user(), now, &inv, "REVOKED", None)
            .await?
            .to_json(now),
        _ => return Err(invalid()),
    };
    txn.commit().await?;
    Ok(respond(StatusCode::OK, doc))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TokenBody {
    pub token: String,
}

/// The PENDING invitation of `token`, with its organization row locked first.
async fn locked_by_token(
    txn: &DatabaseTransaction,
    token: &str,
) -> Result<(store::OrgRow, InvitationRow), ApiError> {
    if token.is_empty() || token.len() > 256 {
        return Err(invalid());
    }
    let hash = token_hash(token);
    let found: InvitationRow = db::one(
        txn,
        &format!("SELECT {INVITATION_COLUMNS} FROM organization_invitations WHERE token_hash = $1"),
        vec![hash.clone().into()],
    )
    .await?
    .ok_or_else(invalid)?;
    let org = store::org(txn, found.organization_id, true)
        .await?
        .ok_or_else(invalid)?;
    let inv: InvitationRow = db::one(
        txn,
        &format!("SELECT {INVITATION_COLUMNS} FROM organization_invitations WHERE id = $1 AND token_hash = $2 FOR UPDATE"),
        vec![found.id.into(), hash.into()],
    )
    .await?
    .ok_or_else(invalid)?;
    if inv.status != "PENDING" {
        return Err(invalid());
    }
    Ok((org, inv))
}

/// Records the expiry (committed) and answers `INVITE_EXPIRED`.
async fn expire(
    txn: DatabaseTransaction,
    ctx: &OpContext,
    now: DateTime<Utc>,
    inv: &InvitationRow,
) -> ApiResult<Response> {
    decide(&txn, ctx, now, inv, "EXPIRED", None).await?;
    txn.commit().await?;
    Ok(ApiError::new(ErrorCode::InviteExpired).into_response())
}

fn email_mismatch() -> ApiError {
    ApiError::with_message(
        ErrorCode::PermissionDenied,
        "The invitation was issued to another verified email address.",
    )
}

/// `POST /internal/v1/organization-invitations/accept` — `{token}` for the acting user
/// whose verified email must equal the invited one. Single use.
pub async fn accept(
    State(state): State<AppState>,
    caller: Caller,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationWrite)?;
    let user = caller.user()?;
    let (req, raw): (TokenBody, Value) = parse(&body)?;
    let verified = caller.verified_email.clone().ok_or_else(|| {
        ApiError::with_message(
            ErrorCode::PermissionDenied,
            "A verified email address (X-CF-Acting-User-Verified-Email) is required to accept an invitation.",
        )
    })?;
    let idem = idempotency::from_headers(
        &headers,
        format!("invitation.accept:{}:{user}", caller.service()),
        &raw,
    )?;
    let txn = state.db.begin().await?;
    if let Some(i) = &idem {
        if let Some(stored) = idempotency::begin(&txn, i).await? {
            return Ok(respond(
                StatusCode::from_u16(stored.response_status as u16).unwrap_or(StatusCode::OK),
                stored.response_body,
            ));
        }
    }
    let (org, inv) = locked_by_token(&txn, &req.token).await?;
    if inv.email_normalized != verified {
        return Err(email_mismatch());
    }
    let now = db::now(&txn).await?;
    let ctx = caller.as_user();
    if inv.expires_at <= now {
        return expire(txn, &ctx, now, &inv).await;
    }
    match org.status() {
        LifecycleStatus::Active => {}
        LifecycleStatus::Suspended => return Err(ApiError::new(ErrorCode::OrganizationSuspended)),
        LifecycleStatus::Archived => return Err(ApiError::new(ErrorCode::OrganizationArchived)),
    }
    // An existing (non-removed) membership is kept unchanged: an invitation never changes
    // an existing role.
    let membership = match store::current_membership(&txn, org.id, user, true).await? {
        Some(m) => m,
        None => {
            let m: MembershipRow = db::one(
                &txn,
                &format!(
                    "INSERT INTO organization_memberships (id, organization_id, user_id, role, status, joined_via, created_by, created_at, updated_at, version) \
                     VALUES ($1, $2, $3, $4, 'ACTIVE', 'INVITATION', $5, $6, $6, 1) RETURNING {MEMBERSHIP_COLUMNS}"
                ),
                vec![Uuid::new_v4().into(), org.id.into(), user.into(), inv.role.clone().into(), inv.invited_by.into(), now.into()],
            )
            .await?
            .ok_or(ApiError::new(ErrorCode::ServiceUnavailable))?;
            store::history(&txn, &ctx, now, None, &m, "CREATED").await?;
            store::emit_membership_activated(&txn, &ctx, now, &m).await?;
            m
        }
    };
    decide(&txn, &ctx, now, &inv, "ACCEPTED", Some(user)).await?;
    let doc = membership.to_json();
    if let Some(i) = &idem {
        idempotency::finish(&txn, i, 200, &doc, Some(membership.id), now).await?;
    }
    txn.commit().await?;
    Ok(respond(StatusCode::OK, doc))
}

/// `POST /internal/v1/organization-invitations/decline` — `{token}`; a supplied verified
/// email must match the invited one.
pub async fn decline(
    State(state): State<AppState>,
    caller: Caller,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationWrite)?;
    let user = caller.user()?;
    let (req, _): (TokenBody, Value) = parse(&body)?;
    let txn = state.db.begin().await?;
    let (_, inv) = locked_by_token(&txn, &req.token).await?;
    if caller
        .verified_email
        .as_ref()
        .is_some_and(|e| *e != inv.email_normalized)
    {
        return Err(email_mismatch());
    }
    let now = db::now(&txn).await?;
    let ctx = caller.as_user();
    if inv.expires_at <= now {
        return expire(txn, &ctx, now, &inv).await;
    }
    let row = decide(&txn, &ctx, now, &inv, "DECLINED", Some(user)).await?;
    txn.commit().await?;
    Ok(respond(StatusCode::OK, row.to_json(now)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_and_only_their_digest_is_kept() {
        let (a, ha) = new_token();
        let (b, hb) = new_token();
        assert_ne!(a, b);
        assert_eq!(a.len(), 43);
        assert_eq!(ha, token_hash(&a));
        assert_ne!(ha, hb);
        assert_eq!(ha.len(), 64);
        assert!(!ha.contains(&a));
    }
}
