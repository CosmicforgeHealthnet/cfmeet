//! Call queues (Part F §56, §47–§55): configuration and the authoritative queue view.
//!
//! A queue holds waiting canonical Calls in FIFO order and offers each, one agent at a
//! time, to an available ACTIVE agent for `offerTimeoutSeconds`. A Call waits at most
//! `maxWaitSeconds`; a full queue (`capacity`) or an expired wait sends the Call to the
//! overflow destination (extension, ring group, another queue, IVR, voicemail) — or ends
//! it (BUSY when full, NO_ANSWER after the wait). Agents are principals of the scope; a
//! PAUSED agent stays a member but receives no offers.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    audit::{self, AuditDetails, AuditFact, AuditOperation, AuditResource},
    check_version, clean_name,
    departments::active_principal,
    destinations::{self, Destination, DestinationType},
    exec,
    policy::{self, AdminCapability, ResourceStatus},
    scope_values, stmt, time, AdminActor, EnterpriseError, EnterpriseResult, EnterpriseService,
    PageRequest,
};
use crate::calls::command::CallScope;

/// Maximum agents of one queue.
pub const MAX_QUEUE_MEMBERS: usize = 100;

text_enum!(
    /// Whether an agent receives offers.
    AgentStatus {
        Active = "ACTIVE",
        Paused = "PAUSED",
    }
);

text_enum!(
    /// Lifecycle of one waiting Call in a queue. WAITING / OFFERING are active; every
    /// other state is final.
    QueueEntryState {
        Waiting = "WAITING",
        Offering = "OFFERING",
        Answered = "ANSWERED",
        Abandoned = "ABANDONED",
        TimedOut = "TIMED_OUT",
        Overflowed = "OVERFLOWED",
        Cancelled = "CANCELLED",
    }
);

/// Kinds a queue's overflow may be (as configured kinds become available).
pub fn overflow_types() -> &'static [DestinationType] {
    &[
        DestinationType::Extension,
        DestinationType::RingGroup,
        DestinationType::Queue,
        DestinationType::Ivr,
    ]
}

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct QueueRow {
    pub id: Uuid,
    pub name: String,
    pub max_wait_seconds: i32,
    pub offer_timeout_seconds: i32,
    pub capacity: i32,
    pub overflow_type: Option<String>,
    pub overflow_id: Option<Uuid>,
    pub status: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub disabled_at: Option<DateTime<Utc>>,
}

impl QueueRow {
    pub(crate) fn overflow(&self) -> Option<Destination> {
        Some(Destination::new(
            DestinationType::parse(self.overflow_type.as_deref()?)?,
            self.overflow_id?,
        ))
    }
}

const COLUMNS: &str =
    "id, name, max_wait_seconds, offer_timeout_seconds, capacity, overflow_type, \
    overflow_id, status, version, created_at, updated_at, disabled_at";

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct QueueMemberRow {
    pub position: i16,
    pub principal_id: Uuid,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueueMember {
    pub principal_id: Uuid,
    #[serde(default = "active")]
    pub status: AgentStatus,
}

fn active() -> AgentStatus {
    AgentStatus::Active
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueMemberView {
    pub position: i16,
    pub principal_id: Uuid,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueView {
    pub queue_id: Uuid,
    pub name: String,
    pub max_wait_seconds: i32,
    pub offer_timeout_seconds: i32,
    pub capacity: i32,
    pub overflow: Option<Destination>,
    pub members: Vec<QueueMemberView>,
    pub waiting: i64,
    pub status: String,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
    pub disabled_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuePage {
    pub queues: Vec<QueueView>,
    pub next_cursor: Option<String>,
}

/// One active (or recently finished) entry as the administrator sees it: never the
/// caller's contact data — only the Call, the position and the timing.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueEntryView {
    pub entry_id: Uuid,
    pub call_id: Uuid,
    pub state: String,
    pub position: Option<i64>,
    pub entered_at: String,
    pub deadline_at: String,
    pub offer_count: i32,
    pub assigned_principal_id: Option<Uuid>,
    pub exited_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueEntryPage {
    pub entries: Vec<QueueEntryView>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QueueSettings {
    pub name: String,
    pub max_wait_seconds: i32,
    pub offer_timeout_seconds: i32,
    pub capacity: i32,
    pub overflow: Option<Destination>,
}

#[derive(Debug, Clone, Default)]
pub struct QueueChange {
    pub name: Option<String>,
    pub max_wait_seconds: Option<i32>,
    pub offer_timeout_seconds: Option<i32>,
    pub capacity: Option<i32>,
    /// `Some(None)` removes the overflow.
    pub overflow: Option<Option<Destination>>,
}

fn validate_limits(max_wait: i32, offer: i32, capacity: i32) -> EnterpriseResult<()> {
    if !(10..=3600).contains(&max_wait) {
        return Err(EnterpriseError::Validation(
            "maxWaitSeconds must be between 10 and 3600.",
        ));
    }
    if !(5..=120).contains(&offer) || offer > max_wait {
        return Err(EnterpriseError::Validation(
            "offerTimeoutSeconds must be between 5 and 120 and at most maxWaitSeconds.",
        ));
    }
    if !(1..=1000).contains(&capacity) {
        return Err(EnterpriseError::Validation(
            "capacity must be between 1 and 1000.",
        ));
    }
    Ok(())
}

pub(crate) async fn queue_row<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    id: Uuid,
    lock: bool,
) -> EnterpriseResult<QueueRow> {
    let mut values = scope_values(scope);
    values.push(id.into());
    QueueRow::find_by_statement(stmt(
        &format!(
            "SELECT {COLUMNS} FROM enterprise_call_queues \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND id = $4{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        values,
    ))
    .one(db)
    .await?
    .ok_or(EnterpriseError::NotFound)
}

/// The queue's agents in position order.
pub(crate) async fn members<C: ConnectionTrait>(
    db: &C,
    queue_id: Uuid,
) -> EnterpriseResult<Vec<QueueMemberRow>> {
    Ok(QueueMemberRow::find_by_statement(stmt(
        "SELECT position, principal_id, status FROM enterprise_queue_members \
         WHERE queue_id = $1 ORDER BY position",
        vec![queue_id.into()],
    ))
    .all(db)
    .await?)
}

/// Active entries of the queue.
pub(crate) async fn waiting_count<C: ConnectionTrait>(
    db: &C,
    queue_id: Uuid,
) -> EnterpriseResult<i64> {
    let row = db
        .query_one(stmt(
            "SELECT count(*) FROM enterprise_queue_entries \
             WHERE queue_id = $1 AND state IN ('WAITING', 'OFFERING')",
            vec![queue_id.into()],
        ))
        .await?;
    Ok(row
        .map(|r| r.try_get_by_index::<i64>(0))
        .transpose()?
        .unwrap_or(0))
}

async fn view<C: ConnectionTrait>(db: &C, row: QueueRow) -> EnterpriseResult<QueueView> {
    let members = members(db, row.id)
        .await?
        .into_iter()
        .map(|m| QueueMemberView {
            position: m.position,
            principal_id: m.principal_id,
            status: m.status,
        })
        .collect();
    let waiting = waiting_count(db, row.id).await?;
    Ok(QueueView {
        overflow: row.overflow(),
        queue_id: row.id,
        name: row.name,
        max_wait_seconds: row.max_wait_seconds,
        offer_timeout_seconds: row.offer_timeout_seconds,
        capacity: row.capacity,
        members,
        waiting,
        status: row.status,
        version: row.version,
        created_at: time(row.created_at),
        updated_at: time(row.updated_at),
        disabled_at: row.disabled_at.map(time),
    })
}

#[derive(Debug, Clone, FromQueryResult)]
struct EntryRow {
    id: Uuid,
    call_id: Uuid,
    state: String,
    position: Option<i64>,
    entered_at: DateTime<Utc>,
    deadline_at: DateTime<Utc>,
    offer_count: i32,
    assigned_principal_id: Option<Uuid>,
    exited_at: Option<DateTime<Utc>>,
}

impl EnterpriseService {
    pub async fn create_queue(
        &self,
        actor: &AdminActor,
        settings: QueueSettings,
    ) -> EnterpriseResult<QueueView> {
        let name = clean_name(&settings.name, 100, "name must be 1-100 characters.")?;
        validate_limits(
            settings.max_wait_seconds,
            settings.offer_timeout_seconds,
            settings.capacity,
        )?;
        let txn = self.begin().await?;
        policy::require_capability(&txn, actor.scope, AdminCapability::ManageRouting).await?;
        if let Some(overflow) = settings.overflow {
            destinations::validate(&txn, actor.scope, overflow, overflow_types()).await?;
        }
        let id = Uuid::new_v4();
        let now = self.now();
        let mut values = vec![id.into()];
        values.extend(scope_values(actor.scope));
        values.extend([
            name.into(),
            settings.max_wait_seconds.into(),
            settings.offer_timeout_seconds.into(),
            settings.capacity.into(),
            settings.overflow.map(|f| f.kind.as_str()).into(),
            settings.overflow.map(|f| f.id).into(),
            now.into(),
        ]);
        exec(
            &txn,
            "INSERT INTO enterprise_call_queues (id, tenant_id, application_id, environment_id, name, \
             max_wait_seconds, offer_timeout_seconds, capacity, overflow_type, overflow_id, status, \
             version, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'ACTIVE', 1, $11, $11)",
            values,
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::Queue,
                resource_id: id,
                operation: AuditOperation::Create,
                old_version: None,
                new_version: Some(1),
                details: AuditDetails::new()
                    .with("capacity", settings.capacity as usize)
                    .with("overflow", settings.overflow.is_some()),
            },
            now,
        )
        .await?;
        let row = queue_row(&txn, actor.scope, id, false).await?;
        let out = view(&txn, row).await?;
        txn.commit().await?;
        crate::metrics::inc(
            "calling_enterprise_config_changes_total",
            &[("resource", "QUEUE"), ("operation", "CREATE")],
        );
        Ok(out)
    }

    pub async fn get_queue(&self, scope: CallScope, id: Uuid) -> EnterpriseResult<QueueView> {
        let row = queue_row(self.db(), scope, id, false).await?;
        view(self.db(), row).await
    }

    pub async fn list_queues(
        &self,
        scope: CallScope,
        status: Option<ResourceStatus>,
        page: &PageRequest,
    ) -> EnterpriseResult<QueuePage> {
        let limit = page.limit()?;
        let mut values = scope_values(scope);
        let mut sql = format!(
            "SELECT {COLUMNS} FROM enterprise_call_queues \
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
        let mut rows = QueueRow::find_by_statement(stmt(&sql, values))
            .all(self.db())
            .await?;
        let next_cursor = if rows.len() as u64 > limit {
            rows.truncate(limit as usize);
            rows.last()
                .map(|r| super::cursor::encode(r.created_at, r.id))
        } else {
            None
        };
        let mut queues = Vec::with_capacity(rows.len());
        for row in rows {
            queues.push(view(self.db(), row).await?);
        }
        Ok(QueuePage {
            queues,
            next_cursor,
        })
    }

    pub async fn update_queue(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
        change: QueueChange,
    ) -> EnterpriseResult<QueueView> {
        let name = change
            .name
            .as_deref()
            .map(|n| clean_name(n, 100, "name must be 1-100 characters."))
            .transpose()?;
        let txn = self.begin().await?;
        policy::require_capability(&txn, actor.scope, AdminCapability::ManageRouting).await?;
        let row = queue_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status != "ACTIVE" {
            return Err(EnterpriseError::Conflict(
                "a disabled queue cannot be changed",
            ));
        }
        let max_wait = change.max_wait_seconds.unwrap_or(row.max_wait_seconds);
        let offer = change
            .offer_timeout_seconds
            .unwrap_or(row.offer_timeout_seconds);
        let capacity = change.capacity.unwrap_or(row.capacity);
        validate_limits(max_wait, offer, capacity)?;
        let overflow = change.overflow.unwrap_or(row.overflow());
        if let Some(o) = overflow {
            if o.id == id {
                return Err(EnterpriseError::Validation(
                    "a queue cannot overflow to itself.",
                ));
            }
            destinations::validate(&txn, actor.scope, o, overflow_types()).await?;
        }
        let now = self.now();
        exec(
            &txn,
            "UPDATE enterprise_call_queues SET name = $2, max_wait_seconds = $3, \
             offer_timeout_seconds = $4, capacity = $5, overflow_type = $6, overflow_id = $7, \
             version = version + 1, updated_at = GREATEST(updated_at, $8) WHERE id = $1",
            vec![
                id.into(),
                name.unwrap_or(row.name.clone()).into(),
                max_wait.into(),
                offer.into(),
                capacity.into(),
                overflow.map(|f| f.kind.as_str()).into(),
                overflow.map(|f| f.id).into(),
                now.into(),
            ],
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::Queue,
                resource_id: id,
                operation: AuditOperation::Update,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new()
                    .with("capacity", capacity as usize)
                    .with("overflow", overflow.is_some()),
            },
            now,
        )
        .await?;
        let row = queue_row(&txn, actor.scope, id, false).await?;
        let out = view(&txn, row).await?;
        txn.commit().await?;
        crate::metrics::inc(
            "calling_enterprise_config_changes_total",
            &[("resource", "QUEUE"), ("operation", "UPDATE")],
        );
        Ok(out)
    }

    /// Replaces the agents atomically (bounded, distinct, ACTIVE principals of the scope).
    /// Waiting Calls are unaffected; a removed agent's pending offer simply expires.
    pub async fn replace_queue_members(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
        members_in: Vec<QueueMember>,
    ) -> EnterpriseResult<QueueView> {
        if members_in.len() > MAX_QUEUE_MEMBERS {
            return Err(EnterpriseError::Validation(
                "a queue has at most 100 agents.",
            ));
        }
        let mut distinct: Vec<Uuid> = members_in.iter().map(|m| m.principal_id).collect();
        distinct.sort();
        distinct.dedup();
        if distinct.len() != members_in.len() {
            return Err(EnterpriseError::Validation("agents must be distinct."));
        }
        let txn = self.begin().await?;
        policy::require_capability(&txn, actor.scope, AdminCapability::ManageRouting).await?;
        let row = queue_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status != "ACTIVE" {
            return Err(EnterpriseError::Conflict(
                "a disabled queue cannot be changed",
            ));
        }
        for member in &members_in {
            if !active_principal(&txn, actor.scope, member.principal_id).await? {
                return Err(EnterpriseError::NotFound);
            }
        }
        let now = self.now();
        exec(
            &txn,
            "DELETE FROM enterprise_queue_members WHERE queue_id = $1",
            vec![id.into()],
        )
        .await?;
        for (i, member) in members_in.iter().enumerate() {
            let mut values = scope_values(actor.scope);
            values.extend([
                id.into(),
                ((i + 1) as i16).into(),
                member.principal_id.into(),
                member.status.as_str().into(),
                now.into(),
            ]);
            exec(
                &txn,
                "INSERT INTO enterprise_queue_members (tenant_id, application_id, environment_id, \
                 queue_id, position, principal_id, status, added_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                values,
            )
            .await?;
        }
        exec(
            &txn,
            "UPDATE enterprise_call_queues SET version = version + 1, \
             updated_at = GREATEST(updated_at, $2) WHERE id = $1",
            vec![id.into(), now.into()],
        )
        .await?;
        let active = members_in
            .iter()
            .filter(|m| m.status == AgentStatus::Active)
            .count();
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::Queue,
                resource_id: id,
                operation: AuditOperation::ReplaceMembers,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new()
                    .with("member_count", members_in.len())
                    .with("active_count", active),
            },
            now,
        )
        .await?;
        let row = queue_row(&txn, actor.scope, id, false).await?;
        let out = view(&txn, row).await?;
        txn.commit().await?;
        Ok(out)
    }

    /// Disables a queue: no new Call enters it; Calls already waiting keep being offered
    /// until they are answered, overflow or time out (never stranded).
    pub async fn disable_queue(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
    ) -> EnterpriseResult<QueueView> {
        let txn = self.begin().await?;
        let row = queue_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status == "DISABLED" {
            return Err(EnterpriseError::Conflict("the queue is already disabled"));
        }
        let now = self.now();
        exec(
            &txn,
            "UPDATE enterprise_call_queues SET status = 'DISABLED', disabled_at = $2, \
             version = version + 1, updated_at = GREATEST(updated_at, $2) WHERE id = $1",
            vec![id.into(), now.into()],
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::Queue,
                resource_id: id,
                operation: AuditOperation::Disable,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new(),
            },
            now,
        )
        .await?;
        let row = queue_row(&txn, actor.scope, id, false).await?;
        let out = view(&txn, row).await?;
        txn.commit().await?;
        Ok(out)
    }

    /// The queue's active entries in FIFO order with their authoritative positions
    /// (1 = next to be offered), or the most recent finished ones with `state`.
    pub async fn list_queue_entries(
        &self,
        scope: CallScope,
        id: Uuid,
        active_only: bool,
        limit: u64,
    ) -> EnterpriseResult<QueueEntryPage> {
        queue_row(self.db(), scope, id, false).await?;
        let sql = if active_only {
            "SELECT id, call_id, state, \
                    row_number() OVER (ORDER BY entered_at, seq) AS position, \
                    entered_at, deadline_at, offer_count, assigned_principal_id, exited_at \
               FROM enterprise_queue_entries \
              WHERE queue_id = $1 AND state IN ('WAITING', 'OFFERING') \
              ORDER BY entered_at, seq LIMIT $2"
        } else {
            "SELECT id, call_id, state, NULL::bigint AS position, entered_at, deadline_at, \
                    offer_count, assigned_principal_id, exited_at \
               FROM enterprise_queue_entries \
              WHERE queue_id = $1 AND state NOT IN ('WAITING', 'OFFERING') \
              ORDER BY exited_at DESC, seq DESC LIMIT $2"
        };
        let rows = EntryRow::find_by_statement(stmt(sql, vec![id.into(), (limit as i64).into()]))
            .all(self.db())
            .await?;
        Ok(QueueEntryPage {
            entries: rows
                .into_iter()
                .map(|r| QueueEntryView {
                    entry_id: r.id,
                    call_id: r.call_id,
                    state: r.state,
                    position: r.position,
                    entered_at: time(r.entered_at),
                    deadline_at: time(r.deadline_at),
                    offer_count: r.offer_count,
                    assigned_principal_id: r.assigned_principal_id,
                    exited_at: r.exited_at.map(time),
                })
                .collect(),
        })
    }
}
