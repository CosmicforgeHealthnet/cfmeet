//! Context persistence. Every lookup is confined to the caller's [`RelayScope`]: a
//! context of another tenant / application / environment is indistinguishable from a
//! nonexistent one.

use chrono::{DateTime, Utc};
use sea_orm::ConnectionTrait;
use uuid::Uuid;

use super::{
    participants::SubjectRef,
    policy::{ContextPolicy, CONTEXT_POLICY_VERSION},
};
use crate::{
    db,
    error::RelayResult,
    models::{
        ChannelRow, ContextRow, DirectionRow, ParticipantRow, CONTEXT_COLUMNS, DIRECTION_COLUMNS,
        PARTICIPANT_COLUMNS,
    },
    scope::RelayScope,
    vocabulary::{CommunicationChannel, DirectionPolicy, RelayStrategy},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lock {
    None,
    Share,
    Update,
}

impl Lock {
    fn clause(&self) -> &'static str {
        match self {
            Lock::None => "",
            Lock::Share => " FOR SHARE",
            Lock::Update => " FOR UPDATE",
        }
    }
}

pub async fn context<C: ConnectionTrait>(
    db: &C,
    scope: RelayScope,
    id: Uuid,
    lock: Lock,
) -> RelayResult<Option<ContextRow>> {
    db::one(
        db,
        &format!(
            "SELECT {CONTEXT_COLUMNS} FROM communication_contexts \
             WHERE id = $1 AND tenant_id = $2 AND application_id = $3 AND environment_id = $4{}",
            lock.clause()
        ),
        vec![
            id.into(),
            scope.tenant_id().into(),
            scope.application_id().into(),
            scope.environment_id().into(),
        ],
    )
    .await
}

pub async fn context_by_reference<C: ConnectionTrait>(
    db: &C,
    scope: RelayScope,
    reference: &str,
) -> RelayResult<Option<ContextRow>> {
    db::one(
        db,
        &format!(
            "SELECT {CONTEXT_COLUMNS} FROM communication_contexts \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
               AND external_context_reference = $4"
        ),
        vec![
            scope.tenant_id().into(),
            scope.application_id().into(),
            scope.environment_id().into(),
            reference.into(),
        ],
    )
    .await
}

pub async fn participants<C: ConnectionTrait>(
    db: &C,
    context_id: Uuid,
    active_only: bool,
) -> RelayResult<Vec<ParticipantRow>> {
    db::all(
        db,
        &format!(
            "SELECT {PARTICIPANT_COLUMNS} FROM communication_context_participants \
             WHERE context_id = $1{} ORDER BY added_in_version, added_at, id",
            if active_only {
                " AND removed_at IS NULL"
            } else {
                ""
            }
        ),
        vec![context_id.into()],
    )
    .await
}

pub async fn channels<C: ConnectionTrait>(
    db: &C,
    context_id: Uuid,
) -> RelayResult<Vec<ChannelRow>> {
    db::all(
        db,
        "SELECT channel, enabled, changed_in_version FROM communication_context_channels \
         WHERE context_id = $1 ORDER BY channel",
        vec![context_id.into()],
    )
    .await
}

pub async fn directions<C: ConnectionTrait>(
    db: &C,
    context_id: Uuid,
    active_only: bool,
) -> RelayResult<Vec<DirectionRow>> {
    db::all(
        db,
        &format!(
            "SELECT {DIRECTION_COLUMNS} FROM communication_context_directions \
             WHERE context_id = $1{} ORDER BY added_in_version, created_at, id",
            if active_only {
                " AND removed_at IS NULL"
            } else {
                ""
            }
        ),
        vec![context_id.into()],
    )
    .await
}

/// Inputs of a new context row.
pub struct NewContext<'a> {
    pub id: Uuid,
    pub environment_kind: &'static str,
    pub external_context_reference: &'a str,
    pub direction_policy: DirectionPolicy,
    pub strategies: &'a [RelayStrategy],
    pub starts_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub grace_expires_at: Option<DateTime<Utc>>,
    pub policy: ContextPolicy,
    pub created_by_service_account_id: Uuid,
}

pub async fn insert_context<C: ConnectionTrait>(
    db: &C,
    scope: RelayScope,
    new: NewContext<'_>,
    now: DateTime<Utc>,
) -> RelayResult<()> {
    db::exec(
        db,
        "INSERT INTO communication_contexts (id, tenant_id, application_id, environment_id, environment_kind, \
           external_context_reference, version, status, direction_policy, allowed_relay_strategies, \
           starts_at, expires_at, grace_expires_at, policy_version, policy_snapshot, \
           created_by_service_account_id, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, 1, 'ACTIVE', $7, $8, $9, $10, $11, $12, $13, $14, $15, $15)",
        vec![
            new.id.into(),
            scope.tenant_id().into(),
            scope.application_id().into(),
            scope.environment_id().into(),
            new.environment_kind.into(),
            new.external_context_reference.into(),
            new.direction_policy.as_str().into(),
            strategy_values(new.strategies).into(),
            new.starts_at.into(),
            new.expires_at.into(),
            new.grace_expires_at.into(),
            CONTEXT_POLICY_VERSION.into(),
            new.policy.snapshot().into(),
            new.created_by_service_account_id.into(),
            now.into(),
        ],
    )
    .await?;
    Ok(())
}

pub fn strategy_values(strategies: &[RelayStrategy]) -> Vec<String> {
    strategies.iter().map(|s| s.as_str().to_string()).collect()
}

pub async fn insert_participant<C: ConnectionTrait>(
    db: &C,
    scope: RelayScope,
    context_id: Uuid,
    subject: SubjectRef,
    version: i64,
    now: DateTime<Utc>,
) -> RelayResult<Uuid> {
    let id = Uuid::new_v4();
    db::exec(
        db,
        "INSERT INTO communication_context_participants (id, context_id, tenant_id, application_id, \
           environment_id, subject_kind, principal_ref, contact_ref, added_in_version, added_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        vec![
            id.into(),
            context_id.into(),
            scope.tenant_id().into(),
            scope.application_id().into(),
            scope.environment_id().into(),
            subject.kind().as_str().into(),
            subject.principal().into(),
            subject.contact().into(),
            version.into(),
            now.into(),
        ],
    )
    .await?;
    Ok(id)
}

pub async fn remove_participant<C: ConnectionTrait>(
    db: &C,
    participant_id: Uuid,
    version: i64,
    now: DateTime<Utc>,
) -> RelayResult<()> {
    db::exec(
        db,
        "UPDATE communication_context_participants SET removed_in_version = $2, removed_at = $3 \
         WHERE id = $1 AND removed_at IS NULL",
        vec![participant_id.into(), version.into(), now.into()],
    )
    .await?;
    Ok(())
}

pub async fn set_channel<C: ConnectionTrait>(
    db: &C,
    scope: RelayScope,
    context_id: Uuid,
    channel: CommunicationChannel,
    enabled: bool,
    version: i64,
    now: DateTime<Utc>,
) -> RelayResult<()> {
    db::exec(
        db,
        "INSERT INTO communication_context_channels (context_id, tenant_id, application_id, environment_id, \
           channel, enabled, changed_in_version, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8) \
         ON CONFLICT (context_id, channel) DO UPDATE \
           SET enabled = EXCLUDED.enabled, changed_in_version = EXCLUDED.changed_in_version, \
               updated_at = EXCLUDED.updated_at",
        vec![
            context_id.into(),
            scope.tenant_id().into(),
            scope.application_id().into(),
            scope.environment_id().into(),
            channel.as_str().into(),
            enabled.into(),
            version.into(),
            now.into(),
        ],
    )
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_direction<C: ConnectionTrait>(
    db: &C,
    scope: RelayScope,
    context_id: Uuid,
    from: Uuid,
    to: Uuid,
    channel: Option<CommunicationChannel>,
    version: i64,
    now: DateTime<Utc>,
) -> RelayResult<Uuid> {
    let id = Uuid::new_v4();
    db::exec(
        db,
        "INSERT INTO communication_context_directions (id, context_id, tenant_id, application_id, \
           environment_id, from_participant_id, to_participant_id, channel, added_in_version, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        vec![
            id.into(),
            context_id.into(),
            scope.tenant_id().into(),
            scope.application_id().into(),
            scope.environment_id().into(),
            from.into(),
            to.into(),
            channel.map(|c| c.as_str().to_string()).into(),
            version.into(),
            now.into(),
        ],
    )
    .await?;
    Ok(id)
}

pub async fn remove_direction<C: ConnectionTrait>(
    db: &C,
    direction_id: Uuid,
    version: i64,
    now: DateTime<Utc>,
) -> RelayResult<()> {
    db::exec(
        db,
        "UPDATE communication_context_directions SET removed_in_version = $2, removed_at = $3 \
         WHERE id = $1 AND removed_at IS NULL",
        vec![direction_id.into(), version.into(), now.into()],
    )
    .await?;
    Ok(())
}
