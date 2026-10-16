//! Organizational number pools (Part IV Workstream J §144.3).
//!
//! A pool groups an organization's numbers (organization-owned assignments of the pool's
//! own scope — database-enforced) with usage / subscription policy references that apply
//! to its members. A member is "assigned" while its current routing points at a target
//! and "available" while unrouted; an assignment is in at most one pool, and an internal
//! transfer carries its membership to the successor. Membership history is kept (a
//! membership is ended, never deleted). Queue, ring-group and business-hours behaviour
//! stay in Calling: a pool stores opaque references only.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, FromQueryResult, TransactionTrait};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use super::{
    assignments::AssignmentRow, routing::RoutingProfileRow, stmt, Actor, DomainError, DomainResult,
    NumberDomain, OwnerScope,
};
use crate::{
    domain::RequestContext,
    infra::outbox::{self, NewEvent},
};

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub name: String,
    pub status: String,
    pub usage_policy_ref: Option<Uuid>,
    pub subscription_policy_ref: Option<Uuid>,
    pub revision: i64,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl PoolRow {
    fn in_scope(&self, scope: &OwnerScope) -> bool {
        self.tenant_id == scope.tenant_id
            && self.application_id == scope.application_id
            && self.environment_id == scope.environment_id
    }
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolMemberRow {
    pub id: Uuid,
    pub pool_id: Uuid,
    pub assignment_id: Uuid,
    pub number_resource_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub added_by: String,
    pub added_at: DateTime<Utc>,
    pub removed_by: Option<String>,
    pub removed_at: Option<DateTime<Utc>>,
}

/// A current member with its assignment and current routing.
#[derive(Debug, Clone)]
pub struct PoolMember {
    pub member: PoolMemberRow,
    pub assignment: AssignmentRow,
    pub routing: Option<RoutingProfileRow>,
}

impl PoolMember {
    /// Routed to a target (voice or SMS) — otherwise available for (re)assignment.
    pub fn assigned(&self) -> bool {
        self.routing
            .as_ref()
            .is_some_and(|r| r.voice_target_kind != "NONE" || r.sms_target_kind != "NONE")
    }
}

async fn pool_event<C: ConnectionTrait>(
    db: &C,
    ctx: &RequestContext,
    pool: &PoolRow,
    change: &str,
    assignment_id: Option<Uuid>,
    now: DateTime<Utc>,
) -> DomainResult<()> {
    outbox::enqueue(
        db,
        NewEvent {
            event_type: "number.pool.changed",
            aggregate_type: "NUMBER_POOL",
            aggregate_id: pool.id,
            scope: Some(crate::infra::outbox::EventScope {
                tenant_id: pool.tenant_id,
                application_id: pool.application_id,
                environment_id: pool.environment_id,
            }),
            payload: json!({
                "pool_id": pool.id,
                "change": change,
                "assignment_id": assignment_id,
                "revision": pool.revision,
                "status": pool.status,
            }),
        },
        ctx,
        now,
    )
    .await?;
    Ok(())
}

impl NumberDomain {
    /// Creates a pool in the trusted scope (names are unique among active pools).
    pub async fn create_pool(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        actor: &Actor,
        name: &str,
        usage_policy_ref: Option<Uuid>,
        subscription_policy_ref: Option<Uuid>,
    ) -> DomainResult<PoolRow> {
        let name = name.trim();
        if name.is_empty() || name.len() > 100 || name.chars().any(char::is_control) {
            return Err(DomainError::Validation(
                "a pool name is 1–100 characters".into(),
            ));
        }
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let row = PoolRow::find_by_statement(stmt(
            "INSERT INTO number_pools (id, tenant_id, application_id, environment_id, name, status, usage_policy_ref, \
               subscription_policy_ref, created_by, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, 'ACTIVE', $6, $7, $8, $9, $9) RETURNING *",
            vec![
                Uuid::new_v4().into(),
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
                name.into(),
                usage_policy_ref.into(),
                subscription_policy_ref.into(),
                actor.reference.clone().into(),
                now.into(),
            ],
        ))
        .one(&txn)
        .await
        .map_err(|e| {
            let e: DomainError = e.into();
            match e {
                DomainError::Conflict { .. } => {
                    DomainError::conflict("POOL_EXISTS", "an active pool has this name")
                }
                other => other,
            }
        })?
        .expect("insert returns the row");
        pool_event(&txn, ctx, &row, "CREATED", None, now).await?;
        txn.commit().await?;
        Ok(row)
    }

    pub async fn pool(&self, scope: &OwnerScope, id: Uuid) -> DomainResult<PoolRow> {
        PoolRow::find_by_statement(stmt(
            "SELECT * FROM number_pools WHERE id = $1",
            vec![id.into()],
        ))
        .one(&self.db)
        .await?
        .filter(|p| p.in_scope(scope))
        .ok_or(DomainError::NotFound("pool"))
    }

    pub async fn pools(&self, scope: &OwnerScope) -> DomainResult<Vec<PoolRow>> {
        Ok(PoolRow::find_by_statement(stmt(
            "SELECT * FROM number_pools WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
             ORDER BY created_at, id LIMIT 200",
            vec![scope.tenant_id.into(), scope.application_id.into(), scope.environment_id.into()],
        ))
        .all(&self.db)
        .await?)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn add_pool_member_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        scope: &OwnerScope,
        actor: &Actor,
        pool_id: Uuid,
        assignment: &AssignmentRow,
        now: DateTime<Utc>,
    ) -> DomainResult<PoolMemberRow> {
        let pool = PoolRow::find_by_statement(stmt(
            "SELECT * FROM number_pools WHERE id = $1 FOR UPDATE",
            vec![pool_id.into()],
        ))
        .one(txn)
        .await?
        .filter(|p| p.in_scope(scope))
        .ok_or(DomainError::NotFound("pool"))?;
        if pool.status != "ACTIVE" {
            return Err(DomainError::precondition(
                "POOL_ARCHIVED",
                "the pool is archived",
            ));
        }
        if !assignment.in_scope(scope) || assignment.status == "CLOSED" {
            return Err(DomainError::NotFound("assignment"));
        }
        if assignment.owner_kind != "ORGANIZATION" {
            return Err(DomainError::precondition(
                "POOL_MEMBER_NOT_ORGANIZATIONAL",
                "only organization-owned numbers join a pool",
            ));
        }
        let member = PoolMemberRow::find_by_statement(stmt(
            "INSERT INTO number_pool_members (id, pool_id, assignment_id, number_resource_id, tenant_id, application_id, \
               environment_id, added_by, added_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING *",
            vec![
                Uuid::new_v4().into(),
                pool.id.into(),
                assignment.id.into(),
                assignment.number_resource_id.into(),
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
                actor.reference.clone().into(),
                now.into(),
            ],
        ))
        .one(txn)
        .await
        .map_err(|e| {
            let e: DomainError = e.into();
            match e {
                DomainError::Conflict { .. } => DomainError::conflict(
                    "POOL_MEMBER_EXISTS",
                    "the number is already in a pool",
                ),
                other => other,
            }
        })?
        .expect("insert returns the row");
        pool_event(txn, ctx, &pool, "MEMBER_ADDED", Some(assignment.id), now).await?;
        Ok(member)
    }

    /// Adds an organization-owned assignment of the scope to a pool.
    pub async fn add_pool_member(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        actor: &Actor,
        pool_id: Uuid,
        assignment_id: Uuid,
    ) -> DomainResult<PoolMemberRow> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let assignment = self.lock_assignment_in(&txn, scope, assignment_id).await?;
        let m = self
            .add_pool_member_in(&txn, ctx, scope, actor, pool_id, &assignment, now)
            .await?;
        txn.commit().await?;
        Ok(m)
    }

    /// Ends a current membership (history kept).
    pub async fn remove_pool_member(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        actor: &Actor,
        pool_id: Uuid,
        assignment_id: Uuid,
    ) -> DomainResult<PoolMemberRow> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let pool = PoolRow::find_by_statement(stmt(
            "SELECT * FROM number_pools WHERE id = $1 FOR UPDATE",
            vec![pool_id.into()],
        ))
        .one(&txn)
        .await?
        .filter(|p| p.in_scope(scope))
        .ok_or(DomainError::NotFound("pool"))?;
        let ended = PoolMemberRow::find_by_statement(stmt(
            "UPDATE number_pool_members SET removed_at = $3, removed_by = $4 \
             WHERE pool_id = $1 AND assignment_id = $2 AND removed_at IS NULL RETURNING *",
            vec![
                pool.id.into(),
                assignment_id.into(),
                now.into(),
                actor.reference.clone().into(),
            ],
        ))
        .one(&txn)
        .await?
        .ok_or(DomainError::NotFound("pool member"))?;
        pool_event(&txn, ctx, &pool, "MEMBER_REMOVED", Some(assignment_id), now).await?;
        txn.commit().await?;
        Ok(ended)
    }

    /// Carries a pool membership to the successor of an internal transfer.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn transfer_pool_membership_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        scope: &OwnerScope,
        actor: &Actor,
        from: &AssignmentRow,
        to: &AssignmentRow,
        now: DateTime<Utc>,
    ) -> DomainResult<()> {
        let Some(old) = PoolMemberRow::find_by_statement(stmt(
            "UPDATE number_pool_members SET removed_at = $2, removed_by = $3 \
             WHERE assignment_id = $1 AND removed_at IS NULL RETURNING *",
            vec![from.id.into(), now.into(), actor.reference.clone().into()],
        ))
        .one(txn)
        .await?
        else {
            return Ok(());
        };
        if to.owner_kind != "ORGANIZATION" {
            return Ok(());
        }
        self.add_pool_member_in(txn, ctx, scope, actor, old.pool_id, to, now)
            .await?;
        Ok(())
    }

    /// The current members of a pool of the scope, with their assignment and routing.
    pub async fn pool_members(
        &self,
        scope: &OwnerScope,
        pool_id: Uuid,
    ) -> DomainResult<Vec<PoolMember>> {
        self.pool(scope, pool_id).await?;
        let members = PoolMemberRow::find_by_statement(stmt(
            "SELECT * FROM number_pool_members WHERE pool_id = $1 AND removed_at IS NULL ORDER BY added_at, id",
            vec![pool_id.into()],
        ))
        .all(&self.db)
        .await?;
        let mut out = Vec::with_capacity(members.len());
        for m in members {
            let assignment = self.assignment(scope, m.assignment_id).await?;
            let routing = match (
                assignment.routing_profile_id,
                assignment.routing_profile_revision,
            ) {
                (Some(p), Some(r)) => Some(self.routing_profile_revision(p, r).await?),
                _ => None,
            };
            out.push(PoolMember {
                member: m,
                assignment,
                routing,
            });
        }
        Ok(out)
    }
}
