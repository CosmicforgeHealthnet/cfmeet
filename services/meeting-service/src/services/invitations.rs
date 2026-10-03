//! Invitation service — hash-only bearer invitations (spec §B6, §C24).
//!
//! The raw token is generated from the OS CSPRNG and returned once; only SHA-256 is
//! stored. Use is consumed with one guarded `UPDATE … RETURNING`, so concurrent use
//! can never exceed `use_limit`, and revoked/expired invitations can never be used.

use chrono::{Duration, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter,
    QueryOrder, Set, Statement, TransactionTrait,
};
use serde_json::json;
use uuid::Uuid;

use crate::{
    domain::{InvitationStatus, Role, SpaceStatus},
    dto::v2::CreateInvitationRequest,
    error::{ApiError, ApiResult, ErrorCode},
    models::{conference_sessions, meeting_invitations, meeting_spaces},
    services::{
        actor::ActorContext,
        meeting_core::{
            find_space, lock_space, permission_denied, space_role, state_conflict, validation, Core,
        },
        meeting_events::{action, audit, AuditActor, AuditRecord},
        meeting_permissions::{check_grant, space_allows, PermissionContext, SpacePermission},
        secrets,
    },
};

pub fn invalid() -> ApiError {
    ApiError::domain(ErrorCode::InvitationInvalid, "This invitation is not valid")
}

/// Pure usability check (shared by resolve and join).
pub fn check_usable(inv: &meeting_invitations::Model, now: chrono::DateTime<Utc>) -> ApiResult<()> {
    if inv.status == InvitationStatus::Revoked {
        return Err(ApiError::domain(
            ErrorCode::InvitationInvalid,
            "This invitation has been revoked",
        ));
    }
    if inv.expires_at <= now {
        return Err(ApiError::domain(
            ErrorCode::InvitationExpired,
            "This invitation has expired",
        ));
    }
    if inv.use_limit.map(|l| inv.usage_count >= l).unwrap_or(false) {
        return Err(ApiError::domain(
            ErrorCode::InvitationInvalid,
            "This invitation has already been used",
        ));
    }
    Ok(())
}

pub struct InvitationService {
    core: Core,
}

impl InvitationService {
    pub fn new(core: Core) -> Self {
        Self { core }
    }

    async fn inviter_role<C: ConnectionTrait>(
        &self,
        conn: &C,
        ctx: &ActorContext,
        space: &meeting_spaces::Model,
    ) -> ApiResult<Role> {
        let actor = ctx.require_registered()?;
        let role = space_role(conn, space, actor).await?.map(|(r, _)| r);
        let pctx = PermissionContext::for_space(space);
        match role {
            Some(r) if space_allows(r, SpacePermission::Invite, &pctx) => Ok(r),
            _ => Err(permission_denied(
                "You are not permitted to invite people to this meeting",
            )),
        }
    }

    /// Returns the invitation and the raw token (shown once).
    pub async fn create(
        &self,
        ctx: &ActorContext,
        space_id: Uuid,
        req: CreateInvitationRequest,
    ) -> ApiResult<(meeting_invitations::Model, String)> {
        let actor = ctx.require_registered()?;
        self.core.require_v2_write(actor)?;
        let hours = req.expires_in_hours.unwrap_or(168);
        if hours < 1 || hours > self.core.config.invitation_max_ttl_hours {
            return Err(validation(format!(
                "expiresInHours must be between 1 and {}",
                self.core.config.invitation_max_ttl_hours
            )));
        }
        if let Some(l) = req.use_limit {
            if !(1..=10_000).contains(&l) {
                return Err(validation("useLimit must be between 1 and 10000"));
            }
        }
        let invitee_email = match &req.invitee_email {
            Some(e) => {
                let e = e.trim().to_ascii_lowercase();
                if !e.contains('@') || e.len() > 320 {
                    return Err(validation("inviteeEmail is not a valid email address"));
                }
                Some(e)
            }
            None => None,
        };
        let requested_role = req.role.unwrap_or(Role::Participant);

        let txn = self.core.db.begin().await?;
        let space = lock_space(&txn, space_id).await?;
        if space.status != SpaceStatus::Active {
            return Err(state_conflict(
                "Archived meeting spaces cannot issue invitations",
            ));
        }
        let inviter_role = self.inviter_role(&txn, ctx, &space).await?;
        check_grant(
            inviter_role,
            requested_role,
            &PermissionContext::for_space(&space),
        )
        .map_err(|d| permission_denied(d.message()))?;
        if let Some(cid) = req.conference_id {
            let scoped = conference_sessions::Entity::find_by_id(cid)
                .filter(conference_sessions::Column::MeetingSpaceId.eq(space_id))
                .one(&txn)
                .await?;
            if scoped.is_none() {
                return Err(ApiError::domain(
                    ErrorCode::ConferenceNotFound,
                    "Conference not found in this meeting",
                ));
            }
        }
        let token = secrets::generate_bearer_secret("inv_");
        let now = Utc::now();
        let invitation = meeting_invitations::ActiveModel {
            id: Set(Uuid::new_v4()),
            meeting_space_id: Set(space_id),
            conference_id: Set(req.conference_id),
            inviter_user_id: Set(actor.user_id),
            invitee_email: Set(invitee_email),
            requested_role: Set(requested_role),
            token_hash: Set(secrets::hash_secret(&token)),
            expires_at: Set(now + Duration::hours(hours)),
            use_limit: Set(req.use_limit),
            usage_count: Set(0),
            status: Set(InvitationStatus::Active),
            revoked_at: Set(None),
            revoked_by: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&txn)
        .await?;
        audit(
            &txn,
            &AuditActor::from(ctx),
            AuditRecord {
                meeting_space_id: space_id,
                conference_id: req.conference_id,
                action: action::INVITATION_CREATE,
                target_type: Some("INVITATION"),
                target_ref: Some(invitation.id.to_string()),
                metadata: json!({
                    "requestedRole": requested_role,
                    "useLimit": invitation.use_limit,
                    "expiresAt": invitation.expires_at,
                    "emailBound": invitation.invitee_email.is_some(),
                }),
            },
        )
        .await?;
        txn.commit().await?;
        Ok((invitation, token))
    }

    pub async fn list(
        &self,
        ctx: &ActorContext,
        space_id: Uuid,
    ) -> ApiResult<Vec<meeting_invitations::Model>> {
        let space = find_space(&self.core.db, space_id).await?;
        self.inviter_role(&self.core.db, ctx, &space).await?;
        Ok(meeting_invitations::Entity::find()
            .filter(meeting_invitations::Column::MeetingSpaceId.eq(space_id))
            .order_by_desc(meeting_invitations::Column::CreatedAt)
            .all(&self.core.db)
            .await?)
    }

    /// Look up a raw token. Unknown tokens are indistinguishable from revoked ones.
    pub async fn find_by_token<C: ConnectionTrait>(
        &self,
        conn: &C,
        token: &str,
    ) -> ApiResult<meeting_invitations::Model> {
        if token.is_empty() || token.len() > 128 {
            return Err(invalid());
        }
        meeting_invitations::Entity::find()
            .filter(meeting_invitations::Column::TokenHash.eq(secrets::hash_secret(token)))
            .one(conn)
            .await?
            .ok_or_else(invalid)
    }

    /// Validate an invitation for joining `conference_id` of `space_id` (no consumption).
    pub async fn validate_for_join(
        &self,
        ctx: &ActorContext,
        token: &str,
        space_id: Uuid,
        conference_id: Uuid,
    ) -> ApiResult<meeting_invitations::Model> {
        let inv = self.find_by_token(&self.core.db, token).await?;
        if inv.meeting_space_id != space_id
            || inv
                .conference_id
                .map(|c| c != conference_id)
                .unwrap_or(false)
        {
            return Err(invalid());
        }
        check_usable(&inv, Utc::now())?;
        if let Some(email) = &inv.invitee_email {
            let verified = ctx.registered().and_then(|r| r.verified_email.clone());
            if verified.as_deref() != Some(email.as_str()) {
                return Err(ApiError::domain(
                    ErrorCode::InvitationInvalid,
                    "This invitation is bound to a different verified account",
                ));
            }
        }
        Ok(inv)
    }

    /// `POST /api/v2/invitations/{token}/resolve` — read-only; does not consume a use.
    pub async fn resolve(
        &self,
        token: &str,
    ) -> ApiResult<(meeting_invitations::Model, meeting_spaces::Model)> {
        let inv = self.find_by_token(&self.core.db, token).await?;
        check_usable(&inv, Utc::now())?;
        let space = find_space(&self.core.db, inv.meeting_space_id)
            .await
            .map_err(|_| invalid())?;
        if space.status != SpaceStatus::Active {
            return Err(invalid());
        }
        Ok((inv, space))
    }

    pub async fn revoke(
        &self,
        ctx: &ActorContext,
        space_id: Uuid,
        invitation_id: Uuid,
    ) -> ApiResult<meeting_invitations::Model> {
        let actor = ctx.require_registered()?;
        let txn = self.core.db.begin().await?;
        let space = lock_space(&txn, space_id).await?;
        self.inviter_role(&txn, ctx, &space).await?;
        let inv = meeting_invitations::Entity::find_by_id(invitation_id)
            .filter(meeting_invitations::Column::MeetingSpaceId.eq(space_id))
            .one(&txn)
            .await?
            .ok_or_else(invalid)?;
        if inv.status == InvitationStatus::Revoked {
            txn.commit().await?;
            return Ok(inv);
        }
        let now = Utc::now();
        let mut am: meeting_invitations::ActiveModel = inv.into();
        am.status = Set(InvitationStatus::Revoked);
        am.revoked_at = Set(Some(now));
        am.revoked_by = Set(Some(actor.user_id));
        am.updated_at = Set(now);
        let updated = am.update(&txn).await?;
        audit(
            &txn,
            &AuditActor::from(ctx),
            AuditRecord {
                meeting_space_id: space_id,
                conference_id: updated.conference_id,
                action: action::INVITATION_REVOKE,
                target_type: Some("INVITATION"),
                target_ref: Some(invitation_id.to_string()),
                metadata: json!({}),
            },
        )
        .await?;
        txn.commit().await?;
        Ok(updated)
    }
}

/// Atomically consume one use. Fails if revoked, expired or exhausted at commit time.
pub async fn consume<C: ConnectionTrait>(conn: &C, invitation_id: Uuid) -> ApiResult<()> {
    let result = conn
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE meeting_invitations
                  SET usage_count = usage_count + 1, updated_at = now()
                WHERE id = $1 AND status = 'ACTIVE' AND expires_at > now()
                  AND (use_limit IS NULL OR usage_count < use_limit)"#,
            [invitation_id.into()],
        ))
        .await?;
    if result.rows_affected() == 1 {
        Ok(())
    } else {
        Err(ApiError::domain(
            ErrorCode::InvitationInvalid,
            "This invitation can no longer be used",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inv() -> meeting_invitations::Model {
        let now = Utc::now();
        meeting_invitations::Model {
            id: Uuid::new_v4(),
            meeting_space_id: Uuid::new_v4(),
            conference_id: None,
            inviter_user_id: Uuid::new_v4(),
            invitee_email: None,
            requested_role: Role::Participant,
            token_hash: "h".into(),
            expires_at: now + Duration::hours(1),
            use_limit: Some(2),
            usage_count: 0,
            status: InvitationStatus::Active,
            revoked_at: None,
            revoked_by: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn usability_rules() {
        let now = Utc::now();
        assert!(check_usable(&inv(), now).is_ok());
        let mut i = inv();
        i.status = InvitationStatus::Revoked;
        assert_eq!(
            check_usable(&i, now).unwrap_err().code(),
            Some(ErrorCode::InvitationInvalid)
        );
        let mut i = inv();
        i.expires_at = now - Duration::seconds(1);
        assert_eq!(
            check_usable(&i, now).unwrap_err().code(),
            Some(ErrorCode::InvitationExpired)
        );
        let mut i = inv();
        i.usage_count = 2;
        assert_eq!(
            check_usable(&i, now).unwrap_err().code(),
            Some(ErrorCode::InvitationInvalid)
        );
        let mut i = inv();
        i.use_limit = None;
        i.usage_count = 1000;
        assert!(check_usable(&i, now).is_ok());
    }
}
