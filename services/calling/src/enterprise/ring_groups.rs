//! Ring groups (Part F §55, §37–§45).
//!
//! A ring group rings its members SIMULTANEOUSLY (bounded by the policy's fan-out) or in
//! ORDER (one member at a time for `memberRingSeconds`, within `groupTimeoutSeconds`),
//! on ONE logical Call. Members are principals, enterprise members, or extensions that
//! route to a principal — never another group, queue or IVR (no nesting, so no cycle).
//! A fallback (extension, queue or voicemail) takes the Call when nobody answers.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    audit::{self, AuditDetails, AuditFact, AuditOperation, AuditResource},
    check_version, clean_name,
    destinations::{self, Destination, DestinationType},
    exec, extensions,
    policy::{self, AdminCapability, ResourceStatus},
    scope_values, stmt, time, AdminActor, EnterpriseError, EnterpriseResult, EnterpriseService,
    PageRequest,
};
use crate::calls::command::CallScope;

text_enum!(
    /// How a ring group rings its members.
    RingStrategy {
        Simultaneous = "SIMULTANEOUS",
        Ordered = "ORDERED",
    }
);

/// Maximum members of a ring group (ordered groups may exceed the simultaneous fan-out).
pub const MAX_RING_GROUP_MEMBERS: usize = 50;

/// Kinds a ring-group member may be.
pub const MEMBER_TYPES: &[DestinationType] = &[
    DestinationType::Principal,
    DestinationType::EnterpriseMember,
    DestinationType::Extension,
];

/// Kinds a ring-group fallback may be (as configured kinds become available).
pub fn fallback_types() -> &'static [DestinationType] {
    &[
        DestinationType::Extension,
        DestinationType::Queue,
        DestinationType::Voicemail,
    ]
}

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct RingGroupRow {
    pub id: Uuid,
    pub name: String,
    pub strategy: String,
    pub member_ring_seconds: i32,
    pub group_timeout_seconds: i32,
    pub fallback_type: Option<String>,
    pub fallback_id: Option<Uuid>,
    pub status: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub disabled_at: Option<DateTime<Utc>>,
}

impl RingGroupRow {
    pub(crate) fn strategy(&self) -> RingStrategy {
        RingStrategy::parse(&self.strategy).unwrap_or(RingStrategy::Simultaneous)
    }

    pub(crate) fn fallback(&self) -> Option<Destination> {
        Some(Destination::new(
            DestinationType::parse(self.fallback_type.as_deref()?)?,
            self.fallback_id?,
        ))
    }
}

const COLUMNS: &str = "id, name, strategy, member_ring_seconds, group_timeout_seconds, \
    fallback_type, fallback_id, status, version, created_at, updated_at, disabled_at";

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct MemberRow {
    pub position: i16,
    pub member_type: String,
    pub member_id: Uuid,
}

impl MemberRow {
    pub(crate) fn destination(&self) -> Option<Destination> {
        Some(Destination::new(
            DestinationType::parse(&self.member_type)?,
            self.member_id,
        ))
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RingGroupMemberView {
    pub position: i16,
    pub member: Destination,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RingGroupView {
    pub ring_group_id: Uuid,
    pub name: String,
    pub strategy: String,
    pub member_ring_seconds: i32,
    pub group_timeout_seconds: i32,
    pub fallback: Option<Destination>,
    pub members: Vec<RingGroupMemberView>,
    pub status: String,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
    pub disabled_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RingGroupPage {
    pub ring_groups: Vec<RingGroupView>,
    pub next_cursor: Option<String>,
}

/// Ring-group settings (create, or a complete change).
#[derive(Debug, Clone, Deserialize)]
pub struct RingGroupSettings {
    pub name: String,
    pub strategy: RingStrategy,
    pub member_ring_seconds: i32,
    pub group_timeout_seconds: i32,
    pub fallback: Option<Destination>,
}

#[derive(Debug, Clone, Default)]
pub struct RingGroupChange {
    pub name: Option<String>,
    pub strategy: Option<RingStrategy>,
    pub member_ring_seconds: Option<i32>,
    pub group_timeout_seconds: Option<i32>,
    /// `Some(None)` removes the fallback.
    pub fallback: Option<Option<Destination>>,
}

fn validate_timing(member: i32, group: i32) -> EnterpriseResult<()> {
    if !(5..=120).contains(&member) {
        return Err(EnterpriseError::Validation(
            "memberRingSeconds must be between 5 and 120.",
        ));
    }
    if !(10..=600).contains(&group) || group < member {
        return Err(EnterpriseError::Validation(
            "groupTimeoutSeconds must be between 10 and 600 and at least memberRingSeconds.",
        ));
    }
    Ok(())
}

pub(crate) async fn ring_group_row<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    id: Uuid,
    lock: bool,
) -> EnterpriseResult<RingGroupRow> {
    let mut values = scope_values(scope);
    values.push(id.into());
    RingGroupRow::find_by_statement(stmt(
        &format!(
            "SELECT {COLUMNS} FROM enterprise_ring_groups \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND id = $4{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        values,
    ))
    .one(db)
    .await?
    .ok_or(EnterpriseError::NotFound)
}

/// The group's members in ring order.
pub(crate) async fn members<C: ConnectionTrait>(
    db: &C,
    ring_group_id: Uuid,
) -> EnterpriseResult<Vec<MemberRow>> {
    Ok(MemberRow::find_by_statement(stmt(
        "SELECT position, member_type, member_id FROM enterprise_ring_group_members \
         WHERE ring_group_id = $1 ORDER BY position",
        vec![ring_group_id.into()],
    ))
    .all(db)
    .await?)
}

async fn view<C: ConnectionTrait>(db: &C, row: RingGroupRow) -> EnterpriseResult<RingGroupView> {
    let members = members(db, row.id)
        .await?
        .into_iter()
        .filter_map(|m| {
            Some(RingGroupMemberView {
                position: m.position,
                member: m.destination()?,
            })
        })
        .collect();
    Ok(RingGroupView {
        fallback: row.fallback(),
        ring_group_id: row.id,
        name: row.name,
        strategy: row.strategy,
        member_ring_seconds: row.member_ring_seconds,
        group_timeout_seconds: row.group_timeout_seconds,
        members,
        status: row.status,
        version: row.version,
        created_at: time(row.created_at),
        updated_at: time(row.updated_at),
        disabled_at: row.disabled_at.map(time),
    })
}

/// Validates one member: an ACTIVE principal / member, or an ACTIVE extension that
/// routes DIRECTLY to a principal (no nesting).
async fn validate_member<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    member: Destination,
) -> EnterpriseResult<()> {
    destinations::validate(db, scope, member, MEMBER_TYPES).await?;
    if member.kind == DestinationType::Extension {
        let ext = extensions::extension_row(db, scope, member.id, false).await?;
        if !ext.destination()?.kind.is_principal() {
            return Err(EnterpriseError::Validation(
                "a ring-group member extension must route directly to a principal (groups do not nest).",
            ));
        }
    }
    Ok(())
}

impl EnterpriseService {
    pub async fn create_ring_group(
        &self,
        actor: &AdminActor,
        settings: RingGroupSettings,
    ) -> EnterpriseResult<RingGroupView> {
        let name = clean_name(&settings.name, 100, "name must be 1-100 characters.")?;
        validate_timing(settings.member_ring_seconds, settings.group_timeout_seconds)?;
        let txn = self.begin().await?;
        policy::require_capability(&txn, actor.scope, AdminCapability::ManageRouting).await?;
        if let Some(fallback) = settings.fallback {
            destinations::validate(&txn, actor.scope, fallback, fallback_types()).await?;
        }
        let id = Uuid::new_v4();
        let now = self.now();
        let mut values = vec![id.into()];
        values.extend(scope_values(actor.scope));
        values.extend([
            name.into(),
            settings.strategy.as_str().into(),
            settings.member_ring_seconds.into(),
            settings.group_timeout_seconds.into(),
            settings.fallback.map(|f| f.kind.as_str()).into(),
            settings.fallback.map(|f| f.id).into(),
            now.into(),
        ]);
        exec(
            &txn,
            "INSERT INTO enterprise_ring_groups (id, tenant_id, application_id, environment_id, name, \
             strategy, member_ring_seconds, group_timeout_seconds, fallback_type, fallback_id, status, \
             version, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'ACTIVE', 1, $11, $11)",
            values,
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::RingGroup,
                resource_id: id,
                operation: AuditOperation::Create,
                old_version: None,
                new_version: Some(1),
                details: AuditDetails::new()
                    .with("strategy", settings.strategy.as_str())
                    .with("fallback", settings.fallback.is_some()),
            },
            now,
        )
        .await?;
        let row = ring_group_row(&txn, actor.scope, id, false).await?;
        let out = view(&txn, row).await?;
        txn.commit().await?;
        Ok(out)
    }

    pub async fn get_ring_group(
        &self,
        scope: CallScope,
        id: Uuid,
    ) -> EnterpriseResult<RingGroupView> {
        let row = ring_group_row(self.db(), scope, id, false).await?;
        view(self.db(), row).await
    }

    pub async fn list_ring_groups(
        &self,
        scope: CallScope,
        status: Option<ResourceStatus>,
        page: &PageRequest,
    ) -> EnterpriseResult<RingGroupPage> {
        let limit = page.limit()?;
        let mut values = scope_values(scope);
        let mut sql = format!(
            "SELECT {COLUMNS} FROM enterprise_ring_groups \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3"
        );
        if let Some(status) = status {
            values.push(status.as_str().into());
            sql.push_str(&format!(" AND status = ${}", values.len()));
        }
        if let Some((at, id)) = page.after()? {
            values.push(at.into());
            values.push(id.into());
            sql.push_str(&format!(
                " AND (created_at, id) > (${}, ${})",
                values.len() - 1,
                values.len()
            ));
        }
        sql.push_str(&format!(" ORDER BY created_at, id LIMIT {}", limit + 1));
        let mut rows = RingGroupRow::find_by_statement(stmt(&sql, values))
            .all(self.db())
            .await?;
        let next_cursor = if rows.len() as u64 > limit {
            rows.truncate(limit as usize);
            rows.last()
                .map(|r| super::cursor::encode(r.created_at, r.id))
        } else {
            None
        };
        let mut ring_groups = Vec::with_capacity(rows.len());
        for row in rows {
            ring_groups.push(view(self.db(), row).await?);
        }
        Ok(RingGroupPage {
            ring_groups,
            next_cursor,
        })
    }

    pub async fn update_ring_group(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
        change: RingGroupChange,
    ) -> EnterpriseResult<RingGroupView> {
        let name = change
            .name
            .as_deref()
            .map(|n| clean_name(n, 100, "name must be 1-100 characters."))
            .transpose()?;
        let txn = self.begin().await?;
        policy::require_capability(&txn, actor.scope, AdminCapability::ManageRouting).await?;
        let row = ring_group_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status != "ACTIVE" {
            return Err(EnterpriseError::Conflict(
                "a disabled ring group cannot be changed",
            ));
        }
        let member = change
            .member_ring_seconds
            .unwrap_or(row.member_ring_seconds);
        let group = change
            .group_timeout_seconds
            .unwrap_or(row.group_timeout_seconds);
        validate_timing(member, group)?;
        let fallback = change.fallback.unwrap_or(row.fallback());
        if let Some(f) = fallback {
            destinations::validate(&txn, actor.scope, f, fallback_types()).await?;
        }
        let strategy = change.strategy.unwrap_or(row.strategy());
        let now = self.now();
        exec(
            &txn,
            "UPDATE enterprise_ring_groups SET name = $2, strategy = $3, member_ring_seconds = $4, \
             group_timeout_seconds = $5, fallback_type = $6, fallback_id = $7, version = version + 1, \
             updated_at = GREATEST(updated_at, $8) WHERE id = $1",
            vec![
                id.into(),
                name.unwrap_or(row.name.clone()).into(),
                strategy.as_str().into(),
                member.into(),
                group.into(),
                fallback.map(|f| f.kind.as_str()).into(),
                fallback.map(|f| f.id).into(),
                now.into(),
            ],
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::RingGroup,
                resource_id: id,
                operation: AuditOperation::Update,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new()
                    .with("strategy", strategy.as_str())
                    .with("fallback", fallback.is_some()),
            },
            now,
        )
        .await?;
        let row = ring_group_row(&txn, actor.scope, id, false).await?;
        let out = view(&txn, row).await?;
        txn.commit().await?;
        Ok(out)
    }

    /// Replaces the ordered member list atomically (bounded, distinct, validated).
    pub async fn replace_ring_group_members(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
        members_in: Vec<Destination>,
    ) -> EnterpriseResult<RingGroupView> {
        if members_in.len() > MAX_RING_GROUP_MEMBERS {
            return Err(EnterpriseError::Validation(
                "a ring group has at most 50 members.",
            ));
        }
        let mut distinct = members_in.clone();
        distinct.sort_by_key(|d| (d.kind, d.id));
        distinct.dedup();
        if distinct.len() != members_in.len() {
            return Err(EnterpriseError::Validation("members must be distinct."));
        }
        let txn = self.begin().await?;
        policy::require_capability(&txn, actor.scope, AdminCapability::ManageRouting).await?;
        let row = ring_group_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status != "ACTIVE" {
            return Err(EnterpriseError::Conflict(
                "a disabled ring group cannot be changed",
            ));
        }
        for member in &members_in {
            validate_member(&txn, actor.scope, *member).await?;
        }
        let now = self.now();
        exec(
            &txn,
            "DELETE FROM enterprise_ring_group_members WHERE ring_group_id = $1",
            vec![id.into()],
        )
        .await?;
        for (i, member) in members_in.iter().enumerate() {
            let mut values = scope_values(actor.scope);
            values.extend([
                id.into(),
                ((i + 1) as i16).into(),
                member.kind.as_str().into(),
                member.id.into(),
                now.into(),
            ]);
            exec(
                &txn,
                "INSERT INTO enterprise_ring_group_members (tenant_id, application_id, \
                 environment_id, ring_group_id, position, member_type, member_id, added_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                values,
            )
            .await?;
        }
        exec(
            &txn,
            "UPDATE enterprise_ring_groups SET version = version + 1, \
             updated_at = GREATEST(updated_at, $2) WHERE id = $1",
            vec![id.into(), now.into()],
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::RingGroup,
                resource_id: id,
                operation: AuditOperation::ReplaceMembers,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new().with("member_count", members_in.len()),
            },
            now,
        )
        .await?;
        let row = ring_group_row(&txn, actor.scope, id, false).await?;
        let out = view(&txn, row).await?;
        txn.commit().await?;
        Ok(out)
    }

    /// Disables a ring group (routing to it then fails safe). Never blocked.
    pub async fn disable_ring_group(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
    ) -> EnterpriseResult<RingGroupView> {
        let txn = self.begin().await?;
        let row = ring_group_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status == "DISABLED" {
            return Err(EnterpriseError::Conflict(
                "the ring group is already disabled",
            ));
        }
        let now = self.now();
        exec(
            &txn,
            "UPDATE enterprise_ring_groups SET status = 'DISABLED', disabled_at = $2, \
             version = version + 1, updated_at = GREATEST(updated_at, $2) WHERE id = $1",
            vec![id.into(), now.into()],
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::RingGroup,
                resource_id: id,
                operation: AuditOperation::Disable,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new(),
            },
            now,
        )
        .await?;
        let row = ring_group_row(&txn, actor.scope, id, false).await?;
        let out = view(&txn, row).await?;
        txn.commit().await?;
        Ok(out)
    }
}
