//! Human workspace members and invitations (Section 50 §6.1).
//!
//! * Only an OWNER manages membership (invite, revoke, change role, remove, transfer
//!   ownership); any member may leave. An organization always keeps at least one OWNER:
//!   every change that could remove the last one locks the OWNER rows and refuses with
//!   LAST_OWNER. Roles are never escalated across tenants: a member id is only ever
//!   resolved inside the authorized tenant.
//! * An invitation carries a one-time token returned ONCE at creation; only its SHA-256
//!   digest is stored. Acceptance requires a verified CF Meet session whose verified
//!   email equals the invited address; expired invitations become EXPIRED.
//! * Memberships are Developer Platform records keyed to the Platform Access tenant id —
//!   not Platform Access principals and not Meeting Service organization members.

use axum::http::StatusCode;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{Duration, Utc};
use rand::RngCore;
use sea_orm::{ConnectionTrait, DatabaseTransaction, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    activity::{self, Event},
    authorize, email,
    pagination::Page,
    stmt, Caller,
};
use crate::{
    domain::rbac::{Action, OrganizationState, Role},
    http::error::{ApiError, ErrorCode},
    state::AppState,
};

/// Invitation lifetime.
pub const INVITATION_TTL_DAYS: i64 = 7;
pub const TOKEN_PREFIX: &str = "cfinv_";

const MEMBER_VIEW: &str = "jsonb_build_object( \
    'id', m.id, 'organizationId', m.tenant_id, 'userId', m.user_id, 'role', m.role, \
    'status', m.status, 'joinedVia', m.joined_via, 'createdAt', m.created_at, \
    'updatedAt', m.updated_at, 'version', m.version)";

const INVITATION_VIEW: &str = "jsonb_build_object( \
    'id', i.id, 'organizationId', i.tenant_id, 'email', i.invitee_email, 'role', i.role, \
    'status', CASE WHEN i.status = 'PENDING' AND i.expires_at <= now() THEN 'EXPIRED' ELSE i.status END, \
    'createdAt', i.created_at, 'expiresAt', i.expires_at, 'decidedAt', i.decided_at)";

fn parse_role(value: &str) -> Result<Role, ApiError> {
    Role::parse(value).ok_or(ApiError(ErrorCode::ValidationError))
}

pub fn token_digest(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn new_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
}

async fn member_view<C: ConnectionTrait>(db: &C, member_id: Uuid) -> Result<Value, ApiError> {
    let row = db
        .query_one(stmt(
            &format!("SELECT {MEMBER_VIEW} AS v FROM devplat_workspace_members m WHERE m.id = $1"),
            vec![member_id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    Ok(row.try_get("", "v")?)
}

/// `GET /v1/organizations/{id}/members`.
pub async fn list(
    state: &AppState,
    caller: &Caller,
    tenant_id: Uuid,
    page: &Page,
) -> Result<Value, ApiError> {
    authorize(state, caller, tenant_id, Action::MembersRead).await?;
    let (at, id) = page
        .cursor_key()
        .map(|(a, i)| (Some(a), Some(i)))
        .unwrap_or((None, None));
    if page.has_cursor() && at.is_none() {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let rows = state
        .db
        .query_all(stmt(
            &format!(
                "SELECT m.created_at AS k_at, m.id AS k_id, {MEMBER_VIEW} AS v \
                 FROM devplat_workspace_members m WHERE m.tenant_id = $1 AND m.status = 'ACTIVE' \
                   AND ($2::timestamptz IS NULL OR (m.created_at, m.id) > ($2, $3)) \
                 ORDER BY m.created_at, m.id LIMIT $4"
            ),
            vec![
                tenant_id.into(),
                at.into(),
                id.into(),
                (page.limit as i64 + 1).into(),
            ],
        ))
        .await?;
    page_of(rows, page)
}

pub(crate) fn page_of(rows: Vec<sea_orm::QueryResult>, page: &Page) -> Result<Value, ApiError> {
    let mut items = Vec::new();
    let mut next = None;
    for r in rows.iter().take(page.limit) {
        items.push(r.try_get::<Value>("", "v")?);
        next = Some(Page::key_cursor(
            r.try_get("", "k_at")?,
            r.try_get("", "k_id")?,
        ));
    }
    Ok(json!({ "items": items, "nextCursor": if rows.len() > page.limit { next } else { None } }))
}

/// Locks the tenant's ACTIVE OWNER rows; returns their member ids.
async fn lock_owners(txn: &DatabaseTransaction, tenant_id: Uuid) -> Result<Vec<Uuid>, ApiError> {
    let rows = txn
        .query_all(stmt(
            "SELECT id FROM devplat_workspace_members \
             WHERE tenant_id = $1 AND status = 'ACTIVE' AND role = 'OWNER' ORDER BY id FOR UPDATE",
            vec![tenant_id.into()],
        ))
        .await?;
    rows.iter()
        .map(|r| r.try_get::<Uuid>("", "id").map_err(ApiError::from))
        .collect()
}

/// The ACTIVE member `member_id` of `tenant_id`, locked (another tenant's member: 404).
async fn lock_member(
    txn: &DatabaseTransaction,
    tenant_id: Uuid,
    member_id: Uuid,
) -> Result<(Uuid, Role, i64), ApiError> {
    let row = txn
        .query_one(stmt(
            "SELECT user_id, role, version FROM devplat_workspace_members \
             WHERE id = $1 AND tenant_id = $2 AND status = 'ACTIVE' FOR UPDATE",
            vec![member_id.into(), tenant_id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    let role: String = row.try_get("", "role")?;
    Ok((
        row.try_get("", "user_id")?,
        Role::parse(&role).ok_or(ApiError(ErrorCode::InternalError))?,
        row.try_get("", "version")?,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangeRole {
    pub role: String,
    #[serde(default)]
    pub expected_version: Option<i64>,
}

/// `PATCH /v1/organizations/{id}/members/{memberId}` (OWNER).
pub async fn change_role(
    state: &AppState,
    caller: &Caller,
    tenant_id: Uuid,
    member_id: Uuid,
    body: ChangeRole,
) -> Result<Value, ApiError> {
    authorize(state, caller, tenant_id, Action::MembersManage).await?;
    let role = parse_role(&body.role)?;
    let txn = state.db.begin().await?;
    let owners = lock_owners(&txn, tenant_id).await?;
    let (_, current, version) = lock_member(&txn, tenant_id, member_id).await?;
    if body.expected_version.is_some_and(|v| v != version) {
        return Err(ApiError(ErrorCode::VersionConflict));
    }
    if current == Role::Owner && role != Role::Owner && owners.len() <= 1 {
        return Err(ApiError(ErrorCode::LastOwner));
    }
    txn.execute(stmt(
        "UPDATE devplat_workspace_members SET role = $2, updated_at = $3, version = version + 1 WHERE id = $1",
        vec![member_id.into(), role.as_str().into(), Utc::now().into()],
    ))
    .await?;
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id,
            action: "member.role_changed",
            target_type: "MEMBER",
            target_id: member_id.to_string(),
            summary: json!({ "fromRole": current.as_str(), "toRole": role.as_str() }),
        },
    )
    .await?;
    let v = member_view(&txn, member_id).await?;
    txn.commit().await?;
    Ok(v)
}

/// `DELETE /v1/organizations/{id}/members/{memberId}`: an OWNER removes a member, or any
/// member removes themself (leave). Never the last OWNER.
pub async fn remove(
    state: &AppState,
    caller: &Caller,
    tenant_id: Uuid,
    member_id: Uuid,
) -> Result<(), ApiError> {
    let human = caller.human()?.clone();
    // Leaving needs only membership; removing someone else needs MembersManage.
    let me = super::membership(&state.db, tenant_id, human.user_id).await?;
    let leaving = me.is_some_and(|(id, _)| id == member_id);
    if leaving {
        let st = super::organization_state(&state.db, tenant_id)
            .await?
            .ok_or(ApiError(ErrorCode::NotFound))?;
        if matches!(st, OrganizationState::Closed) {
            return Err(ApiError(ErrorCode::OrganizationClosed));
        }
    } else {
        authorize(state, caller, tenant_id, Action::MembersManage).await?;
    }
    let txn = state.db.begin().await?;
    let owners = lock_owners(&txn, tenant_id).await?;
    let (_, role, _) = lock_member(&txn, tenant_id, member_id).await?;
    if role == Role::Owner && owners.len() <= 1 {
        return Err(ApiError(ErrorCode::LastOwner));
    }
    let now = Utc::now();
    txn.execute(stmt(
        "UPDATE devplat_workspace_members SET status = 'REMOVED', removed_at = $2, updated_at = $2, \
             version = version + 1 WHERE id = $1",
        vec![member_id.into(), now.into()],
    ))
    .await?;
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id,
            action: if leaving {
                "member.left"
            } else {
                "member.removed"
            },
            target_type: "MEMBER",
            target_id: member_id.to_string(),
            summary: json!({ "role": role.as_str() }),
        },
    )
    .await?;
    txn.commit().await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransferOwnership {
    pub member_id: Uuid,
}

/// `POST /v1/organizations/{id}/ownership-transfer` (OWNER): the target becomes OWNER
/// and the transferring owner becomes ADMIN, atomically.
pub async fn transfer_ownership(
    state: &AppState,
    caller: &Caller,
    tenant_id: Uuid,
    body: TransferOwnership,
) -> Result<Value, ApiError> {
    let access = authorize(state, caller, tenant_id, Action::OwnershipTransfer).await?;
    let (my_member, _) = access.member.ok_or(ApiError(ErrorCode::PermissionDenied))?;
    if my_member == body.member_id {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let txn = state.db.begin().await?;
    lock_owners(&txn, tenant_id).await?;
    let (_, my_role, _) = lock_member(&txn, tenant_id, my_member).await?;
    if my_role != Role::Owner {
        return Err(ApiError(ErrorCode::PermissionDenied));
    }
    let (_, target_role, _) = lock_member(&txn, tenant_id, body.member_id).await?;
    let now = Utc::now();
    txn.execute(stmt(
        "UPDATE devplat_workspace_members SET role = 'OWNER', updated_at = $2, version = version + 1 WHERE id = $1",
        vec![body.member_id.into(), now.into()],
    ))
    .await?;
    txn.execute(stmt(
        "UPDATE devplat_workspace_members SET role = 'ADMIN', updated_at = $2, version = version + 1 WHERE id = $1",
        vec![my_member.into(), now.into()],
    ))
    .await?;
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id,
            action: "organization.ownership_transferred",
            target_type: "MEMBER",
            target_id: body.member_id.to_string(),
            summary: json!({ "fromMemberId": my_member, "toMemberId": body.member_id, "previousRole": target_role.as_str() }),
        },
    )
    .await?;
    let v = member_view(&txn, body.member_id).await?;
    txn.commit().await?;
    Ok(v)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateInvitation {
    pub email: String,
    pub role: String,
}

/// Inserts a PENDING invitation and returns `(view, one-time token)`.
pub async fn insert_invitation<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    invitee: &str,
    role: Role,
    invited_by: &str,
) -> Result<(Value, String), ApiError> {
    let token = new_token();
    let id = Uuid::new_v4();
    let now = Utc::now();
    // An expired PENDING invitation never blocks a new one.
    db.execute(stmt(
        "UPDATE devplat_invitations SET status = 'EXPIRED', decided_at = $3 \
         WHERE tenant_id = $1 AND invitee_email = $2 AND status = 'PENDING' AND expires_at <= $3",
        vec![tenant_id.into(), invitee.into(), now.into()],
    ))
    .await?;
    let inserted = db
        .execute(stmt(
            "INSERT INTO devplat_invitations \
                 (id, tenant_id, invitee_email, role, token_digest, status, invited_by, created_at, expires_at) \
             VALUES ($1, $2, $3, $4, $5, 'PENDING', $6, $7, $8) ON CONFLICT DO NOTHING",
            vec![
                id.into(),
                tenant_id.into(),
                invitee.into(),
                role.as_str().into(),
                token_digest(&token).into(),
                invited_by.into(),
                now.into(),
                (now + Duration::days(INVITATION_TTL_DAYS)).into(),
            ],
        ))
        .await?
        .rows_affected();
    if inserted != 1 {
        return Err(ApiError(ErrorCode::InvitationPending));
    }
    let row = db
        .query_one(stmt(
            &format!("SELECT {INVITATION_VIEW} AS v FROM devplat_invitations i WHERE i.id = $1"),
            vec![id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::InternalError))?;
    Ok((row.try_get("", "v")?, token))
}

/// `POST /v1/organizations/{id}/invitations` (OWNER). The token is returned once.
pub async fn invite(
    state: &AppState,
    caller: &Caller,
    tenant_id: Uuid,
    body: CreateInvitation,
) -> Result<(StatusCode, Value), ApiError> {
    authorize(state, caller, tenant_id, Action::MembersManage).await?;
    caller.human()?;
    let invitee = email(&body.email)?;
    let role = parse_role(&body.role)?;
    let txn = state.db.begin().await?;
    let (mut v, token) =
        insert_invitation(&txn, tenant_id, &invitee, role, &caller.actor_ref()).await?;
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id,
            action: "invitation.created",
            target_type: "INVITATION",
            target_id: v["id"].as_str().unwrap_or_default().to_string(),
            summary: json!({ "role": role.as_str() }),
        },
    )
    .await?;
    txn.commit().await?;
    v["token"] = Value::from(token);
    Ok((StatusCode::CREATED, v))
}

/// Removes the one-time token from an idempotently stored response.
pub fn redact_token(v: &mut Value) {
    if let Some(o) = v.as_object_mut() {
        o.remove("token");
    }
}

/// `GET /v1/organizations/{id}/invitations` (no token, ever).
pub async fn list_invitations(
    state: &AppState,
    caller: &Caller,
    tenant_id: Uuid,
    page: &Page,
) -> Result<Value, ApiError> {
    authorize(state, caller, tenant_id, Action::MembersRead).await?;
    let (at, id) = page
        .cursor_key()
        .map(|(a, i)| (Some(a), Some(i)))
        .unwrap_or((None, None));
    let rows = state
        .db
        .query_all(stmt(
            &format!(
                "SELECT i.created_at AS k_at, i.id AS k_id, {INVITATION_VIEW} AS v \
                 FROM devplat_invitations i WHERE i.tenant_id = $1 \
                   AND ($2::timestamptz IS NULL OR (i.created_at, i.id) > ($2, $3)) \
                 ORDER BY i.created_at, i.id LIMIT $4"
            ),
            vec![
                tenant_id.into(),
                at.into(),
                id.into(),
                (page.limit as i64 + 1).into(),
            ],
        ))
        .await?;
    page_of(rows, page)
}

/// `POST /v1/organizations/{id}/invitations/{invitationId}/revoke` (OWNER).
pub async fn revoke_invitation(
    state: &AppState,
    caller: &Caller,
    tenant_id: Uuid,
    invitation_id: Uuid,
) -> Result<Value, ApiError> {
    authorize(state, caller, tenant_id, Action::MembersManage).await?;
    let human = caller.human()?.clone();
    let txn = state.db.begin().await?;
    let n = txn
        .execute(stmt(
            "UPDATE devplat_invitations SET status = 'REVOKED', decided_at = $3, revoked_by_user_id = $4 \
             WHERE id = $1 AND tenant_id = $2 AND status = 'PENDING' AND expires_at > $3",
            vec![
                invitation_id.into(),
                tenant_id.into(),
                Utc::now().into(),
                human.user_id.into(),
            ],
        ))
        .await?
        .rows_affected();
    if n != 1 {
        let exists = txn
            .query_one(stmt(
                "SELECT 1 AS x FROM devplat_invitations WHERE id = $1 AND tenant_id = $2",
                vec![invitation_id.into(), tenant_id.into()],
            ))
            .await?
            .is_some();
        return Err(ApiError(if exists {
            ErrorCode::InvalidState
        } else {
            ErrorCode::NotFound
        }));
    }
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id,
            action: "invitation.revoked",
            target_type: "INVITATION",
            target_id: invitation_id.to_string(),
            summary: json!({}),
        },
    )
    .await?;
    let row = txn
        .query_one(stmt(
            &format!("SELECT {INVITATION_VIEW} AS v FROM devplat_invitations i WHERE i.id = $1"),
            vec![invitation_id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::InternalError))?;
    let v = row.try_get("", "v")?;
    txn.commit().await?;
    Ok(v)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcceptInvitation {
    pub token: String,
}

/// `POST /v1/invitations/accept`: the invited human accepts with the one-time token.
/// The FIRST owner acceptance of an INVITED (operator-registered) organization activates
/// it.
pub async fn accept(
    state: &AppState,
    caller: &Caller,
    body: AcceptInvitation,
) -> Result<Value, ApiError> {
    let human = caller.human()?.clone();
    let token = body.token.trim();
    if !token.starts_with(TOKEN_PREFIX) || token.len() > 128 {
        return Err(ApiError(ErrorCode::InvitationInvalid));
    }
    let now = Utc::now();
    let txn = state.db.begin().await?;
    let row = txn
        .query_one(stmt(
            "SELECT i.id, i.tenant_id, i.invitee_email, i.role, i.status, i.expires_at, o.state \
             FROM devplat_invitations i JOIN devplat_organizations o ON o.tenant_id = i.tenant_id \
             WHERE i.token_digest = $1 FOR UPDATE OF i",
            vec![token_digest(token).into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::InvitationInvalid))?;
    let id: Uuid = row.try_get("", "id")?;
    let tenant_id: Uuid = row.try_get("", "tenant_id")?;
    let status: String = row.try_get("", "status")?;
    let expires_at: chrono::DateTime<Utc> = row.try_get("", "expires_at")?;
    if status != "PENDING" {
        return Err(ApiError(ErrorCode::InvitationInvalid));
    }
    if expires_at <= now {
        txn.execute(stmt(
            "UPDATE devplat_invitations SET status = 'EXPIRED', decided_at = $2 WHERE id = $1",
            vec![id.into(), now.into()],
        ))
        .await?;
        txn.commit().await?;
        return Err(ApiError(ErrorCode::InvitationInvalid));
    }
    let invitee: String = row.try_get("", "invitee_email")?;
    if human.verified_email.as_deref() != Some(invitee.as_str()) {
        return Err(ApiError(ErrorCode::InvitationEmailMismatch));
    }
    let org_state = OrganizationState::parse(&row.try_get::<String>("", "state")?)
        .ok_or(ApiError(ErrorCode::InternalError))?;
    if matches!(
        org_state,
        OrganizationState::Suspended | OrganizationState::Closed
    ) {
        return Err(ApiError(if org_state == OrganizationState::Closed {
            ErrorCode::OrganizationClosed
        } else {
            ErrorCode::OrganizationSuspended
        }));
    }
    let role = parse_role(&row.try_get::<String>("", "role")?)?;
    let member_id = Uuid::new_v4();
    let inserted = txn
        .execute(stmt(
            "INSERT INTO devplat_workspace_members \
                 (id, tenant_id, user_id, role, status, joined_via, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, 'ACTIVE', 'INVITATION', $5, $5) ON CONFLICT DO NOTHING",
            vec![
                member_id.into(),
                tenant_id.into(),
                human.user_id.into(),
                role.as_str().into(),
                now.into(),
            ],
        ))
        .await?
        .rows_affected();
    if inserted != 1 {
        return Err(ApiError(ErrorCode::AlreadyMember));
    }
    txn.execute(stmt(
        "UPDATE devplat_invitations SET status = 'ACCEPTED', decided_at = $2, accepted_by_user_id = $3 WHERE id = $1",
        vec![id.into(), now.into(), human.user_id.into()],
    ))
    .await?;
    if org_state == OrganizationState::Invited && role == Role::Owner {
        txn.execute(stmt(
            "UPDATE devplat_organizations SET state = 'ACTIVE', state_reason = NULL, updated_at = $2, \
                 version = version + 1 WHERE tenant_id = $1 AND state = 'INVITED'",
            vec![tenant_id.into(), now.into()],
        ))
        .await?;
    }
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id,
            action: "invitation.accepted",
            target_type: "MEMBER",
            target_id: member_id.to_string(),
            summary: json!({ "invitationId": id, "role": role.as_str() }),
        },
    )
    .await?;
    let v = member_view(&txn, member_id).await?;
    txn.commit().await?;
    Ok(v)
}

/// Worker sweep: PENDING invitations past expiry become EXPIRED (immutable afterwards).
pub async fn expire_due<C: ConnectionTrait>(db: &C) -> Result<u64, sea_orm::DbErr> {
    Ok(db
        .execute(stmt(
            "UPDATE devplat_invitations SET status = 'EXPIRED', decided_at = now() \
             WHERE status = 'PENDING' AND expires_at <= now()",
            vec![],
        ))
        .await?
        .rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_and_only_their_digest_is_comparable() {
        let a = new_token();
        let b = new_token();
        assert_ne!(a, b);
        assert!(a.starts_with(TOKEN_PREFIX));
        assert_eq!(token_digest(&a).len(), 64);
        assert_ne!(token_digest(&a), token_digest(&b));
    }
}
