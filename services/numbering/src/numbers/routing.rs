//! Versioned routing profiles (Part IV §D7): where a number's voice and SMS traffic goes,
//! as opaque CF Meet ids of Calling / enterprise constructs (a principal, an endpoint, an
//! extension, a ring group, a queue, an IVR, a department, an application). Numbering never
//! reads those services' data; the targets are references validated by their owners when
//! traffic is authorized (Workstream K).
//!
//! A revision is immutable; a change supersedes the CURRENT revision and adds the next
//! one (`revision + 1`, same scope — database-enforced), so an assignment can pin the exact
//! revision it routes by.

use chrono::{DateTime, Utc};
use sea_orm::{FromQueryResult, TransactionTrait};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use super::{
    stmt, Actor, DomainError, DomainResult, NumberDomain, OwnerScope, SmsTargetKind,
    VoiceTargetKind,
};
use crate::{
    domain::RequestContext,
    infra::outbox::{self, NewEvent},
};

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingProfileRow {
    pub id: Uuid,
    pub profile_id: Uuid,
    pub revision: i64,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub voice_target_kind: String,
    pub voice_target_ref: Option<Uuid>,
    pub sms_target_kind: String,
    pub sms_target_ref: Option<Uuid>,
    pub status: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub superseded_at: Option<DateTime<Utc>>,
}

/// Routing targets of one revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutingTargets {
    pub voice: (VoiceTargetKind, Option<Uuid>),
    pub sms: (SmsTargetKind, Option<Uuid>),
}

impl RoutingTargets {
    fn validate(&self) -> DomainResult<()> {
        if (self.voice.0 == VoiceTargetKind::None) != self.voice.1.is_none()
            || (self.sms.0 == SmsTargetKind::None) != self.sms.1.is_none()
        {
            return Err(DomainError::Validation(
                "a routing target has a reference exactly when its kind is not NONE".into(),
            ));
        }
        Ok(())
    }
}

fn scope_matches(row: &RoutingProfileRow, scope: &OwnerScope) -> bool {
    row.tenant_id == scope.tenant_id
        && row.application_id == scope.application_id
        && row.environment_id == scope.environment_id
}

impl NumberDomain {
    #[allow(clippy::too_many_arguments)]
    async fn insert_revision(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        ctx: &RequestContext,
        profile_id: Uuid,
        revision: i64,
        scope: &OwnerScope,
        targets: RoutingTargets,
        actor: &Actor,
        now: DateTime<Utc>,
    ) -> DomainResult<RoutingProfileRow> {
        let row = RoutingProfileRow::find_by_statement(stmt(
            "INSERT INTO number_routing_profiles (id, profile_id, revision, tenant_id, application_id, environment_id, \
               voice_target_kind, voice_target_ref, sms_target_kind, sms_target_ref, status, created_by, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'CURRENT', $11, $12) RETURNING *",
            vec![
                Uuid::new_v4().into(),
                profile_id.into(),
                revision.into(),
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
                targets.voice.0.as_str().into(),
                targets.voice.1.into(),
                targets.sms.0.as_str().into(),
                targets.sms.1.into(),
                actor.reference.clone().into(),
                now.into(),
            ],
        ))
        .one(txn)
        .await?
        .expect("insert returns the row");
        outbox::enqueue(
            txn,
            NewEvent {
                event_type: "number.routing-profile.revised",
                aggregate_type: "ROUTING_PROFILE",
                aggregate_id: profile_id,
                scope: Some(scope.event_scope()),
                payload: json!({
                    "routing_profile_id": profile_id,
                    "revision": revision,
                    "voice_target_kind": targets.voice.0.as_str(),
                    "sms_target_kind": targets.sms.0.as_str(),
                }),
            },
            ctx,
            now,
        )
        .await?;
        Ok(row)
    }

    /// Creates a routing profile (revision 1) in the trusted scope.
    pub async fn create_routing_profile(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        actor: Actor,
        targets: RoutingTargets,
    ) -> DomainResult<RoutingProfileRow> {
        targets.validate()?;
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let row = self
            .insert_revision(&txn, ctx, Uuid::new_v4(), 1, scope, targets, &actor, now)
            .await?;
        txn.commit().await?;
        Ok(row)
    }

    /// Supersedes the CURRENT revision `expected_revision` and adds the next one.
    pub async fn revise_routing_profile(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        actor: Actor,
        profile_id: Uuid,
        expected_revision: i64,
        targets: RoutingTargets,
    ) -> DomainResult<RoutingProfileRow> {
        targets.validate()?;
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let current = RoutingProfileRow::find_by_statement(stmt(
            "SELECT * FROM number_routing_profiles WHERE profile_id = $1 AND status = 'CURRENT' FOR UPDATE",
            vec![profile_id.into()],
        ))
        .one(&txn)
        .await?
        .filter(|r| scope_matches(r, scope))
        .ok_or(DomainError::NotFound("routing profile"))?;
        if current.revision != expected_revision {
            return Err(DomainError::VersionConflict("routing profile"));
        }
        sea_orm::ConnectionTrait::execute(
            &txn,
            stmt(
                "UPDATE number_routing_profiles SET status = 'SUPERSEDED', superseded_at = $2 WHERE id = $1",
                vec![current.id.into(), now.into()],
            ),
        )
        .await?;
        let next = self
            .insert_revision(
                &txn,
                ctx,
                profile_id,
                current.revision + 1,
                scope,
                targets,
                &actor,
                now,
            )
            .await?;
        txn.commit().await?;
        Ok(next)
    }

    /// The CURRENT revision of a profile in `scope`.
    pub async fn current_routing_profile(
        &self,
        scope: &OwnerScope,
        profile_id: Uuid,
    ) -> DomainResult<RoutingProfileRow> {
        RoutingProfileRow::find_by_statement(stmt(
            "SELECT * FROM number_routing_profiles WHERE profile_id = $1 AND status = 'CURRENT'",
            vec![profile_id.into()],
        ))
        .one(&self.db)
        .await?
        .filter(|r| scope_matches(r, scope))
        .ok_or(DomainError::NotFound("routing profile"))
    }

    /// Every revision of a profile in `scope`, oldest first.
    pub async fn routing_profile_revisions(
        &self,
        scope: &OwnerScope,
        profile_id: Uuid,
    ) -> DomainResult<Vec<RoutingProfileRow>> {
        let rows = RoutingProfileRow::find_by_statement(stmt(
            "SELECT * FROM number_routing_profiles WHERE profile_id = $1 AND tenant_id = $2 AND application_id = $3 \
               AND environment_id = $4 ORDER BY revision",
            vec![
                profile_id.into(),
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
            ],
        ))
        .all(&self.db)
        .await?;
        if rows.is_empty() {
            return Err(DomainError::NotFound("routing profile"));
        }
        Ok(rows)
    }
}
